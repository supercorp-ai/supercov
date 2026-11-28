# frozen_string_literal: true

# Supercov's strong mutation runtime for Ruby: mutant schemata and the
# driver that runs them (docs/design/mutation-drivers.md).
#
# Loaded through RUBYOPT in place of the coverage runtime. It does three
# things:
#
# 1. Compiles each file the pass switches from the copy Rust wrote, where
#    every mutant the pass runs sits behind a switch that reads
#    `Supercov::Strong::MUTANT` (a mutant's key) each time it is evaluated:
#    a constant holding an Integer, which a non-main Ractor can read where
#    it could not read a global.
# 2. Counts the active mutant's evaluations (`Supercov::Strong.h`) and,
#    past the hit limit, raises `Supercov::Strong::MutantTimeout`, an
#    Exception no `rescue => e` catches, and notes the mutant in the
#    timeouts file.
# 3. When the suite has loaded and RSpec, Minitest, test-unit or Cucumber
#    is about to run it, runs the plan instead, as mutant does: one fork of
#    the loaded process per mutant, with the mutant switched on, running its
#    candidate tests and stopping at the first that fails.
#
# It requires nothing outside the standard library.

module Supercov
  module Strong
    PLAN_ENV = "SUPERCOV_STRONG_RUBY_PLAN"
    ACTIVE_ENV = "SUPERCOV_ACTIVE_MUTANT"
    SWITCHES_ONLY_ENV = "SUPERCOV_STRONG_SWITCHES_ONLY"
    TIMEOUT_CODE = "SUPERCOV_MUTANT_TIMEOUT"
    MAIN = Ractor.current
    # The seed a Minitest suite runs with in the pass, unless its command
    # names one: the same order every pass, so an edit that changes no test
    # does not move a verdict.
    SEED = 0
    MUTANT = -1
    # The signals a stop sends the pass (Ctrl-C, the CLI at its budget or
    # when it is itself stopped).
    STOP_SIGNALS = %w[INT TERM].freeze

    # Raised past the hit limit: an Exception, so code under test that
    # rescues StandardError does not carry on through an endless loop.
    class MutantTimeout < Exception # rubocop:disable Lint/InheritException
    end

    @plan = nil
    @hits = 0
    @limit = 1_000_000
    @timed_out = false
    @ran = false

    class << self
      attr_reader :plan
      attr_accessor :ran

      def load_plan(path)
        @plan = Kernel.eval(File.read(path, encoding: "UTF-8"), TOPLEVEL_BINDING.dup, path, 1) # rubocop:disable Security/Eval
        @root = File.realpath(@plan["root"])
        @files = {}
        @plan["files"].each do |relative, switched|
          @files[File.join(@root, relative)] = switched
        end
        @limit = Integer(ENV["SUPERCOV_MUTANT_HIT_LIMIT"] || @plan["hitLimit"] || 1_000_000)
        @keys = {}
        @plan["mutants"].each { |mutant| @keys["#{mutant['file']}##{mutant['index']}"] = mutant["key"] }
        active = ENV[ACTIVE_ENV].to_s
        activate(@keys.fetch(active, -1)) unless active.empty? || active == "-"
      end

      def activate(key)
        remove_const(:MUTANT)
        const_set(:MUTANT, key)
        @hits = 0
        @timed_out = false
        # A Ruby process a test starts (`assert_separately`, a CLI) loads
        # this runtime through RUBYOPT: it switches the same mutant on, as
        # it would load the mutated file from disk.
        ENV[ACTIVE_ENV] = active_name if Strong.ran
      end

      # One evaluation of the active mutant's replacement.
      # (A non-main Ractor cannot count: its evaluations are not limited.)
      def h
        return unless MAIN.equal?(Ractor.current)

        @hits += 1
        return if @hits <= @limit

        unless @timed_out
          @timed_out = true
          note_timeout
        end
        raise MutantTimeout, "#{TIMEOUT_CODE}: the mutant ran past #{@limit} evaluations"
      end

      def timed_out? = @timed_out

      # The pass begins: a Ruby process its tests start switches mutants on
      # and runs its own tests, never a pass of its own.
      def start
        @ran = true
        ENV[SWITCHES_ONLY_ENV] = "1"
      end

      # Whether a switched file was compiled with its switches: one loaded
      # before this runtime (by a gemspec Bundler evaluates, say) was not.
      def compiled?(relative)
        (@compiled || {}).key?(File.join(@root, relative))
      end

      def note_timeout
        path = @plan && @plan["timeouts"]
        return unless path

        File.open(path, "a") { |file| file.write(%({"mutant": #{Json.string(active_name)}, "attemptId": null, "pass": "main"}\n)) }
      rescue StandardError
        nil
      end

      def active_name
        @keys.key(MUTANT) || "-"
      end

      # The switched copy of a file the pass switches, or nil.
      def compile(path)
        return nil unless @files

        absolute = begin
          File.realpath(path)
        rescue StandardError
          return nil
        end
        switched = @files[absolute]
        return nil unless switched

        iseq = RubyVM::InstructionSequence.compile(File.binread(switched), path, path, 1)
        (@compiled ||= {})[absolute] = true
        iseq
      end
    end

    module Json
      module_function

      def string(text)
        out = +'"'
        text.to_s.each_char do |character|
          out << case character
                 when '"' then '\\"'
                 when "\\" then "\\\\"
                 when "\n" then "\\n"
                 when "\r" then "\\r"
                 when "\t" then "\\t"
                 else
                   character.ord < 0x20 ? format("\\u%04x", character.ord) : character
                 end
        end
        out << '"'
      end

      def value(value)
        case value
        when nil then "null"
        when true then "true"
        when false then "false"
        when Integer, Float then value.to_s
        when Hash then "{#{value.map { |key, item| "#{string(key)}: #{value(item)}" }.join(', ')}}"
        when Array then "[#{value.map { |item| value(item) }.join(', ')}]"
        else string(value)
        end
      end
    end

    module Loader
      def load_iseq(path)
        compiled = Supercov::Strong.compile(path)
        return compiled if compiled

        defined?(super) ? super : nil
      end

      def self.install
        target = RubyVM::InstructionSequence.singleton_class
        target.prepend(Loader)
        target.define_singleton_method(:prepend) do |*modules|
          result = super(*modules)
          super(Loader.dup) if modules.any? { |mod| mod.method_defined?(:load_iseq) || mod.private_method_defined?(:load_iseq) }
          result
        end
      end
    end

    # A process forked from the loaded suite that runs jobs, each in a fork
    # of its own: forks are made side by side, not one after another by the
    # pass.
    class Worker
      attr_reader :reader

      def initialize(pass, others)
        @reader, child_writer = IO.pipe
        child_reader, @writer = IO.pipe
        [@reader, child_writer, child_reader, @writer].each(&:binmode)
        @pid = fork do
          @reader.close
          @writer.close
          # The pass's ends of the other workers' pipes: held here, a
          # worker's input would never reach its end.
          others.each(&:close_ends)
          Pass.child do
            while (job = Worker.read(child_reader))
              Worker.write(child_writer, pass.send(:run_job, job))
            end
          end
        end
        child_writer.close
        child_reader.close
      end

      def close_ends
        @reader.close
        @writer.close
      end

      def send_job(job)
        Worker.write(@writer, job.slice(:key, :ids, :all, :limit))
      end

      def receive
        Worker.read(@reader) || { data: nil, killed: false, status: "the worker ended", duration: 0, stopped: stopped? }
      end

      # Whether the worker, its output ended, was ended by a stop's signal.
      def stopped?
        _, status = Process.waitpid2(@pid)
        @pid = nil
        Pass.stopped_by?(status)
      rescue StandardError
        false
      end

      # Ends the worker now: a stop judges nothing it runs.
      def kill
        return unless @pid

        Process.kill("KILL", @pid)
        Process.waitpid(@pid)
      rescue StandardError
        nil
      end

      def stop
        @writer.close
        Process.waitpid(@pid) if @pid
      rescue StandardError
        nil
      end

      def self.write(io, value)
        data = Marshal.dump(value)
        io.write([data.bytesize].pack("N"), data)
        io.flush
      end

      def self.read(io)
        header = io.read(4)
        return nil if header.nil? || header.bytesize < 4

        Marshal.load(io.read(header.unpack1("N"))) # rubocop:disable Security/MarshalLoad
      end
    end

    # The pass: every mutant of the plan, each in a fork of this process.
    class Pass
      # A forked process's work, ended without the parent's `at_exit` hooks
      # (a runner's autorun among them): an error that escapes it ends the
      # process with no outcome written.
      #
      # A stop's signal ends it at once, as the system ends a process: what
      # the suite or the code under test does with the signal (RSpec's
      # first Ctrl-C only asks it to quit; code that rescues every
      # Exception carries on) would let it go on and end as no mutant made
      # it end. Its parent sees the signal and judges nothing it ran.
      def self.child
        STOP_SIGNALS.each { |signal| trap(signal, "SYSTEM_DEFAULT") }
        yield
        exit!(0)
      rescue Exception => error # rubocop:disable Lint/RescueException
        warn("[supercov] #{error.class}: #{error.message}")
        exit!(1)
      end

      def self.stopped_by?(status)
        !status.nil? && status.signaled? && STOP_SIGNALS.include?(Signal.signame(status.termsig))
      end

      def initialize(runner)
        @runner = runner
        @plan = Strong.plan
        @jobs = Integer(ENV["SUPERCOV_STRONG_JOBS"] || @plan["jobs"] || 4)
        @results = File.open(@plan["results"], "a")
        @results.sync = true
        @confirmed = {}
        # The CLI says a stop here (`stopped-at` beside the results) before
        # it signals the pass: from then on the pass starts nothing and
        # judges nothing, and what it had not decided is left for the next.
        @stop_marker = @plan["stopped"]
        @stop = nil
      end

      # Whether the pass has been told to stop: a signal it heard, a process
      # of it that a stop's signal ended, or the CLI's word.
      def stopped?
        return true if @stop

        @stop = "TERM" if @stop_marker && File.exist?(@stop_marker)
        !@stop.nil?
      end

      # A stop ends the pass with the status of its signal.
      def end_stopped(workers)
        workers.each(&:kill)
        @results.close
        exit!(@stop == "INT" ? 130 : 143)
      end

      def run
        # Heard here rather than by the suite: RSpec's own handler only asks
        # its examples to stop, and forks made after it ran none.
        STOP_SIGNALS.each { |signal| trap(signal) { @stop ||= signal } }
        tests = @runner.tests
        baseline = run_isolated(-1, @plan["baseline"].select { |id| tests.key?(id) }, nil, all: true)
        end_stopped([]) if stopped?
        unless baseline
          # Without a baseline no verdict means anything: every mutant is an
          # error, and the pass says why.
          @plan["mutants"].each { |mutant| write(mutant, "error", nil, 0, 0, "the candidate tests did not finish without a mutant") }
          @results.close
          return
        end
        @failing = baseline[:failed].to_h { |id| [id, true] }
        warn("[supercov] strong baseline: #{baseline[:sequence].size} test(s), failing without a mutant: #{baseline[:failed].join(', ')}") if ENV["SUPERCOV_STRONG_DEBUG"]
        @durations = baseline[:durations]
        queue = @plan["mutants"].select do |mutant|
          next true if Strong.compiled?(mutant["file"])

          write(mutant, "error", nil, 0, 0, "its file was loaded before Supercov's runtime, without its switches")
          false
        end
        @queue = queue.map { |mutant| { kind: :mutant, mutant: mutant } }
        # Kills waiting on a confirmation, by the tests they ran.
        @waiting = {}
        @confirmations = 0
        @overturned = 0
        workers = []
        @jobs.times { workers << Worker.new(self, workers.dup) }
        idle = workers.dup
        busy = {}
        until (@queue.empty? && busy.empty?) || stopped?
          while !stopped? && (worker = idle.first) && (job = @queue.shift)
            next unless (sent = dispatch(job, tests))

            idle.shift
            worker.send_job(sent)
            busy[worker.reader] = [worker, sent]
          end
          ready, = IO.select(busy.keys, nil, nil, 0.1)
          (ready || []).each do |reader|
            worker, job = busy.delete(reader)
            idle << worker
            finish(job, worker.receive)
          end
        end
        end_stopped(workers) if stopped?
        workers.each(&:stop)
        @results.write("#{Json.value('summary' => true, 'driver' => 'ruby', 'confirmations' => @confirmations, 'overturned' => @overturned)}\n")
        @results.close
      end

      private

      def candidates(mutant, tests)
        mutant_tests(mutant).select { |id| tests.key?(id) && !@failing[id] }
      end

      # A mutant's candidate tests, in the baseline's order.
      def mutant_tests(mutant)
        @plan["sets"][mutant["set"]].map { |index| @plan["baseline"][index] }
      end

      def limit(ids)
        total = ids.sum { |id| @durations.fetch(id, 1.0) }
        (total * 3) + 10 + Float(@plan["extraSeconds"] || 0)
      end

      # The job a worker runs: a mutant's candidates with it on, or a kill's
      # tests again without it. Nil where there is nothing to run.
      def dispatch(job, tests)
        if job[:kind] == :confirm
          # In the pool, so a confirmation holds a worker, not the pass.
          return job.merge(key: -1, ids: job[:sequence], all: true, limit: limit(job[:sequence]))
        end

        mutant = job[:mutant]
        ids = candidates(mutant, tests)
        if ids.empty?
          write(mutant, "survived", nil, 0, 0, "no candidate test passes without the mutant")
          return nil
        end
        job.merge(key: mutant["key"], ids: ids, all: false, limit: limit(ids))
      end

      # In a worker process: forks the loaded suite for the job, holds the
      # fork to its time limit, and says how it ended.
      def run_job(job)
        reader, writer = IO.pipe
        reader.binmode
        writer.binmode
        started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
        pid = fork do
          reader.close
          Pass.child do
            outcome = attempt(job[:key], job[:ids], all: job[:all])
            writer.write(Marshal.dump(outcome))
            writer.close
          end
        end
        writer.close
        # Read as the fork writes: a large outcome would fill the pipe and
        # hold the fork.
        output = Thread.new { reader.read }
        killed = false
        status = nil
        loop do
          _, status = Process.waitpid2(pid, Process::WNOHANG)
          break if status

          if @stop_marker && File.exist?(@stop_marker)
            Process.kill("KILL", pid) rescue nil # rubocop:disable Style/RescueModifier
            Process.waitpid(pid)
            output.join
            reader.close
            return { data: nil, killed: false, status: "stopped", duration: 0, stopped: true }
          end
          if Process.clock_gettime(Process::CLOCK_MONOTONIC) - started > job[:limit]
            Process.kill("KILL", pid) rescue nil # rubocop:disable Style/RescueModifier
            killed = true
            _, status = Process.waitpid2(pid)
            break
          end
          sleep 0.001
        end
        data = output.value
        reader.close
        duration = ((Process.clock_gettime(Process::CLOCK_MONOTONIC) - started) * 1000).round
        { data: data, killed: killed, status: status.inspect, duration: duration, stopped: !killed && Pass.stopped_by?(status) }
      end

      def finish(job, ended)
        # What a stop ended says nothing of its mutant.
        @stop ||= "TERM" if ended[:stopped]
        return if stopped?

        data = ended[:data]
        duration = ended[:duration]
        outcome = data.nil? || data.empty? ? nil : Marshal.load(data) # rubocop:disable Security/MarshalLoad
        return confirmed(job, outcome) if job[:kind] == :confirm

        mutant = job[:mutant]
        if outcome.nil?
          status_word = ended[:killed] ? "timeout" : "killed"
          reason = ended[:killed] ? "its tests ran past their time" : "the test process ended (#{ended[:status]}) before its tests did"
          write(mutant, status_word, nil, job[:ids].size, duration, reason)
          return
        end
        if outcome[:timed_out]
          write(mutant, "timeout", outcome[:failed].first, outcome[:ran], duration, "the mutant ran past its hit limit")
          return
        end
        killer = outcome[:failed].first
        unless killer
          write(mutant, "survived", nil, outcome[:ran], duration, nil)
          return
        end
        kill = { mutant: mutant, killer: killer, ran: outcome[:ran], duration: duration }
        sequence = outcome[:sequence]
        case @confirmed[sequence]
        when true then write(mutant, "killed", killer, outcome[:ran], duration, nil)
        when false then overturn(kill)
        else
          # The first kill after these tests asks for their confirmation;
          # later ones wait on it.
          unless @waiting.key?(sequence)
            @confirmations += 1
            @queue.unshift({ kind: :confirm, sequence: sequence })
          end
          (@waiting[sequence] ||= []) << kill
        end
      end

      def confirmed(job, outcome)
        sequence = job[:sequence]
        passed = !outcome.nil? && outcome[:failed].empty?
        @confirmed[sequence] = passed
        (@waiting.delete(sequence) || []).each do |kill|
          if passed
            write(kill[:mutant], "killed", kill[:killer], kill[:ran], kill[:duration], nil)
          else
            overturn(kill)
          end
        end
      end

      # A killing test that fails without the mutant after the same tests:
      # the candidates' files run whole, with and without it.
      def overturn(kill)
        @overturned += 1
        files_whole(kill[:mutant], kill[:duration])
      end

      def files_whole(mutant, duration)
        whole = @runner.whole(mutant_tests(mutant))
        without = run_isolated(-1, whole, nil, all: true)
        unless without
          write(mutant, "error", nil, 0, duration, "its tests' files did not finish without it")
          return
        end
        with = run_isolated(mutant["key"], whole, limit(whole), all: true)
        if with.nil?
          write(mutant, "timeout", nil, whole.size, duration, "its tests' files ran past their time")
          return
        end
        killer = (with[:failed] - without[:failed]).first
        status = with[:timed_out] ? "timeout" : (killer ? "killed" : "survived")
        write(mutant, status, killer, with[:ran], duration, "ran its tests' files whole: its tests failed without it after the tests before them")
      end

      # Runs `ids` in a fork with the mutant `key` on; nil where it ran past
      # `deadline` seconds.
      def run_isolated(key, ids, deadline, all:)
        reader, writer = IO.pipe
        reader.binmode
        writer.binmode
        pid = fork do
          reader.close
          Pass.child do
            writer.write(Marshal.dump(attempt(key, ids, all: all)))
            writer.close
          end
        end
        writer.close
        output = Thread.new { reader.read }
        if deadline
          started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
          until (status = Process.waitpid2(pid, Process::WNOHANG)&.last)
            if Process.clock_gettime(Process::CLOCK_MONOTONIC) - started > deadline
              Process.kill("KILL", pid) rescue nil # rubocop:disable Style/RescueModifier
              Process.waitpid(pid)
              output.join
              reader.close
              return nil
            end
            sleep 0.01
          end
        else
          _, status = Process.waitpid2(pid)
        end
        data = output.value
        reader.close
        @stop ||= "TERM" if Pass.stopped_by?(status)
        data.empty? ? nil : Marshal.load(data) # rubocop:disable Security/MarshalLoad
      end

      # In the forked process: the tests, in order, with the mutant on,
      # stopping at the first that fails unless `all`.
      def attempt(key, ids, all:)
        Strong.activate(key)
        outcome = begin
          @runner.run(ids, all)
        rescue MutantTimeout
          { failed: ids.take(1), sequence: ids.take(1), durations: {} }
        end
        outcome.merge(ran: outcome[:sequence].size, timed_out: Strong.timed_out?)
      end

      def write(mutant, status, killed_by, tests, duration, reason)
        return if stopped?

        record = { "id" => mutant["id"], "status" => status, "killedBy" => killed_by, "tests" => tests, "durationMs" => duration, "reason" => reason }
        @results.write("#{Json.value(record)}\n")
      end
    end

    # RSpec: examples by their id (`spec/a_spec.rb[1:2]`).
    class RSpecRunner
      # Notes each example as it finishes.
      class Listener
        attr_reader :finished

        def initialize
          @finished = []
        end

        def example_finished(notification)
          @finished << notification.example
        end
      end

      def tests = @examples

      def whole(ids)
        files = ids.filter_map { |id| @examples[id]&.metadata&.fetch(:rerun_file_path, nil) }.uniq
        @examples.select { |_, example| files.include?(example.metadata[:rerun_file_path]) }.keys
      end

      # What a failure outside any example (a `before(:suite)` hook that
      # raised) is reported as.
      OUTSIDE = "(outside examples)"

      def initialize(runner)
        @runner = runner
        @examples = {}
        ::RSpec.world.all_examples.each { |example| @examples[example.id.sub(%r{\A\./}, "")] = example }
      end

      # Runs the examples `ids` names through RSpec's own `run_specs`, its
      # reporter, formatters and suite hooks as the suite runs them, with
      # output thrown away. Called once per forked process.
      def run(ids, all)
        selected = ids.to_h { |id| [@examples.fetch(id), true] }
        world = ::RSpec.world
        groups = []
        collect = lambda do |group|
          groups << group
          group.children.each(&collect)
        end
        world.example_groups.each(&collect)
        groups.each do |group|
          group.instance_variable_set(:@descendant_filtered_examples, nil)
          world.filtered_examples[group] = group.examples.select { |example| selected[example] }
        end
        configuration = ::RSpec.configuration
        configuration.fail_fast = all ? false : 1
        listener = Listener.new
        configuration.reporter.register_listener(listener, :example_finished)
        $stdout.reopen(File::NULL)
        $stderr.reopen(File::NULL) unless ENV["SUPERCOV_STRONG_DEBUG"]
        # Only the groups that hold a chosen example: every other group would
        # still be announced to the reporter.
        groups = world.ordered_example_groups.reject { |group| group.descendant_filtered_examples.empty? }
        status = @runner.run_specs(groups)
        by_id = @examples.invert
        sequence = listener.finished.map { |example| by_id.fetch(example) }
        failed = listener.finished.select { |example| example.execution_result.status == :failed }.map { |example| by_id.fetch(example) }
        failed << OUTSIDE if failed.empty? && status.is_a?(Integer) && status != 0
        durations = listener.finished.to_h { |example| [by_id.fetch(example), example.execution_result.run_time.to_f] }
        { failed: failed, sequence: sequence, durations: durations }
      end
    end

    # Minitest: tests as `Class#method`.
    class MinitestRunner
      def initialize
        @tests = {}
        ::Minitest::Runnable.runnables.each do |klass|
          klass.runnable_methods.each { |name| @tests["#{klass.name}##{name}"] = [klass, name] }
        end
      end

      def tests = @tests

      def whole(ids)
        classes = ids.filter_map { |id| @tests[id]&.first }.uniq
        @tests.select { |_, (klass, _)| classes.include?(klass) }.keys
      end

      def run(ids, all)
        failed = []
        sequence = []
        durations = {}
        ids.each do |id|
          klass, name = @tests.fetch(id)
          started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
          result = klass.new(name).run
          durations[id] = Process.clock_gettime(Process::CLOCK_MONOTONIC) - started
          sequence << id
          raise MutantTimeout, TIMEOUT_CODE if result.failures.any? { |failure| failure.respond_to?(:error) && failure.error.is_a?(MutantTimeout) }
          next if result.passed? || result.skipped?

          failed << id
          break unless all
        end
        { failed: failed, sequence: sequence, durations: durations }
      end
    end

    # test-unit: tests as `Class#method`, collected as its own runner
    # collects them. A data-driven test runs every data set under its id.
    class TestUnitRunner
      # What a failure outside any test (a class's `startup`) is reported as.
      OUTSIDE = "(outside tests)"

      def initialize(suite)
        @tests = {}
        flatten = lambda do |test|
          if test.is_a?(::Test::Unit::TestSuite)
            test.tests.each(&flatten)
          else
            (@tests["#{test.class.name}##{test.method_name}"] ||= []) << test
          end
        end
        flatten.call(suite)
      end

      def tests = @tests

      def whole(ids)
        classes = ids.filter_map { |id| @tests[id]&.first&.class }.uniq
        @tests.select { |_, (test, *)| classes.include?(test.class) }.keys
      end

      # Runs the tests `ids` names through test-unit's own mediator, a suite
      # per class so each class's `startup` and `shutdown` run around them.
      def run(ids, all)
        failed = []
        sequence = []
        durations = {}
        root = ::Test::Unit::TestSuite.new("supercov")
        ids.group_by { |id| @tests.fetch(id).first.class }.each do |klass, class_ids|
          suite = ::Test::Unit::TestSuite.new(klass.name, klass)
          class_ids.each { |id| @tests.fetch(id).each { |test| suite << test } }
          root << suite
        end
        mediator = ::Test::Unit::UI::TestRunnerMediator.new(root)
        current = nil
        started = nil
        failing = false
        timed_out = false
        mediator.add_listener(::Test::Unit::TestCase::STARTED_OBJECT) do |test|
          current = "#{test.class.name}##{test.method_name}"
          started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
          failing = false
        end
        mediator.add_listener(::Test::Unit::TestResult::FAULT) do |fault|
          next unless fault.is_a?(::Test::Unit::Failure) || fault.is_a?(::Test::Unit::Error)

          timed_out ||= fault.is_a?(::Test::Unit::Error) && fault.exception.is_a?(MutantTimeout)
          failing = true
          # A class's `startup` or `shutdown`, outside any test.
          failed << OUTSIDE if current.nil? && !failed.include?(OUTSIDE)
        end
        mediator.add_listener(::Test::Unit::TestCase::FINISHED_OBJECT) do |_test|
          raise MutantTimeout, TIMEOUT_CODE if timed_out

          durations[current] = (durations[current] || 0) + Process.clock_gettime(Process::CLOCK_MONOTONIC) - started
          sequence << current unless sequence.last == current
          failed << current if failing && !failed.include?(current)
          id = current
          current = nil
          throw :supercov_stop if failing && !all && id
        end
        $stdout.reopen(File::NULL)
        catch(:supercov_stop) { mediator.run }
        raise MutantTimeout, TIMEOUT_CODE if timed_out

        { failed: failed, sequence: sequence, durations: durations }
      end
    end

    # Cucumber: scenarios by their location (`features/a.feature:3`), the
    # feature files compiled through Cucumber's own filters and runner.
    class CucumberRunner
      # Collects the test cases the selecting filters let through.
      class Collect
        attr_reader :cases

        def initialize
          @cases = []
        end

        def test_case(test_case)
          @cases << test_case
          self
        end

        def done = self
      end

      # The filters that choose scenarios (tags, names, lines): the ones
      # before step activation, which run nothing.
      SELECTING = %w[TagLimits TagFilter NameFilter LocationsFilter].freeze

      def initialize(runtime)
        @runtime = runtime
        collect = Collect.new
        selecting = runtime.send(:filters).select { |filter| SELECTING.include?(filter.class.name.to_s.split("::").last) }
        runtime.compile(runtime.send(:features), collect, selecting, ::Cucumber::Core::EventBus.new)
        @tests = collect.cases.to_h { |test_case| [test_case.location.to_s, test_case] }
      end

      def tests = @tests

      def whole(ids)
        files = ids.filter_map { |id| @tests[id]&.location&.file }.uniq
        @tests.select { |_, test_case| files.include?(test_case.location.file) }.keys
      end

      # Cucumber runs the chosen scenarios in the features' order.
      def run(ids, all)
        selected = ids.to_h { |id| [id, true] }
        failed = []
        sequence = []
        durations = {}
        started = nil
        timed_out = false
        configuration = @runtime.configuration
        bus = configuration.event_bus
        bus.on(:test_case_started) { started = Process.clock_gettime(Process::CLOCK_MONOTONIC) }
        bus.on(:test_case_finished) do |event|
          id = event.test_case.location.to_s
          result = event.result
          timed_out ||= result.respond_to?(:exception) && result.exception.is_a?(MutantTimeout)
          raise MutantTimeout, TIMEOUT_CODE if timed_out

          durations[id] = (durations[id] || 0) + Process.clock_gettime(Process::CLOCK_MONOTONIC) - started
          sequence << id unless sequence.last == id
          if result.failed?
            failed << id unless failed.include?(id)
            throw :supercov_stop unless all
          end
        end
        filters = @runtime.send(:filters)
        at = filters.index { |filter| SELECTING.include?(filter.class.name.to_s.split("::").last) && filter.class.name.to_s.end_with?("LocationsFilter") }
        filters.insert(at ? at + 1 : 0, Select.new(selected))
        $stdout.reopen(File::NULL)
        catch(:supercov_stop) { @runtime.compile(@runtime.send(:features), receiver(bus), filters, bus) }
        raise MutantTimeout, TIMEOUT_CODE if timed_out

        { failed: failed, sequence: sequence, durations: durations }
      end

      # Passes on only the chosen test cases.
      class Select
        def initialize(ids, receiver = nil)
          @ids = ids
          @receiver = receiver
        end

        def with_receiver(receiver) = self.class.new(@ids, receiver)

        def test_case(test_case)
          test_case.describe_to(@receiver) if @ids.key?(test_case.location.to_s)
          self
        end

        def done
          @receiver.done
          self
        end
      end

      private

      # Cucumber's test runner, with the options `Runtime#run!` gives it.
      def receiver(bus)
        runner = ::Cucumber::Core::Test::Runner
        keywords = runner.instance_method(:initialize).parameters.filter_map { |kind, name| name if %i[key keyreq].include?(kind) }
        configuration = @runtime.configuration
        options = {}
        options[:id_generator] = configuration.id_generator if keywords.include?(:id_generator)
        options[:backtrace_filter] = ::Cucumber::Formatter::BacktraceFilter if keywords.include?(:backtrace_filter)
        options[:retry_policy] = configuration.retry_policy if keywords.include?(:retry_policy)
        runner.new(bus, **options)
      end
    end

    module RSpecHook
      def run_specs(example_groups)
        return super if Strong.ran || Strong.plan.nil? || ENV[SWITCHES_ONLY_ENV]

        Strong.start
        # Examples in the order they are defined, whatever `--order` says,
        # as mutant runs them: the same order every pass. (Registered on the
        # registry: `register_ordering` does nothing once `--order` is set.)
        ::RSpec.configuration.ordering_registry.register(:global, ::RSpec::Core::Ordering::Identity.new)
        Pass.new(RSpecRunner.new(self)).run
        0
      end
    end

    module MinitestHook
      def run(args = [])
        return super if Strong.ran || Strong.plan.nil? || ENV[SWITCHES_ONLY_ENV]

        Strong.start
        # What `Minitest.run` sets before it runs anything.
        options = respond_to?(:process_args) ? process_args(args) : {}
        given = ENV["SEED"] || args.any? { |arg| arg.start_with?("--seed", "-s") }
        options[:seed] = SEED unless given
        self.seed ||= options[:seed]
        srand(seed)
        init_plugins(options) if respond_to?(:init_plugins)
        Pass.new(MinitestRunner.new).run
        true
      end
    end

    # test-unit's autorunner, from the `at_exit` that `require "test/unit"`
    # sets: its collector gives the suite it would run.
    module TestUnitHook
      def run
        return super if Strong.ran || Strong.plan.nil? || ENV[SWITCHES_ONLY_ENV]

        Strong.start
        self.class.need_auto_run = false
        suite = @collector[self]
        Pass.new(TestUnitRunner.new(suite)).run if suite
        true
      end
    end

    # Cucumber's runtime, once its step definitions and support files have
    # loaded.
    module CucumberHook
      def run!
        return super if Strong.ran || Strong.plan.nil? || ENV[SWITCHES_ONLY_ENV]

        Strong.start
        load_step_definitions
        fire_install_plugin_hook
        Pass.new(CucumberRunner.new(self)).run
      end
    end

    def self.install
      path = ENV[PLAN_ENV].to_s
      return if path.empty?

      load_plan(path)
      Loader.install
      attach
    end

    # The runners' hooks go on as their classes are defined.
    def self.attach
      TracePoint.new(:end) do |trace|
        case trace.self.name
        when "RSpec::Core::Runner" then ::RSpec::Core::Runner.prepend(RSpecHook)
        when "Minitest" then ::Minitest.singleton_class.prepend(MinitestHook)
        when "Test::Unit::AutoRunner" then ::Test::Unit::AutoRunner.prepend(TestUnitHook)
        when "Cucumber::Runtime" then ::Cucumber::Runtime.prepend(CucumberHook)
        end
      end.enable
      ::RSpec::Core::Runner.prepend(RSpecHook) if defined?(::RSpec::Core::Runner)
      ::Minitest.singleton_class.prepend(MinitestHook) if defined?(::Minitest) && ::Minitest.respond_to?(:run)
      ::Test::Unit::AutoRunner.prepend(TestUnitHook) if defined?(::Test::Unit::AutoRunner)
      ::Cucumber::Runtime.prepend(CucumberHook) if defined?(::Cucumber::Runtime)
    end
  end
end

Supercov::Strong.install
