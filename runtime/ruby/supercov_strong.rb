# frozen_string_literal: true

# Supercov's strong mutation runtime for Ruby: mutant schemata and the
# driver that runs them (docs/design/mutation-drivers.md).
#
# Loaded through RUBYOPT in place of the coverage runtime. It does three
# things:
#
# 1. Compiles each file the pass switches from the copy Rust wrote, where
#    every mutant the pass runs sits behind a switch that reads
#    `$__supercov_mutant` (a mutant's key) each time it is evaluated.
# 2. Counts the active mutant's evaluations (`$__supercov_strong.h`) and,
#    past the hit limit, raises `Supercov::Strong::MutantTimeout`, an
#    Exception no `rescue => e` catches, and notes the mutant in the
#    timeouts file.
# 3. When the suite has loaded and RSpec or Minitest is about to run it,
#    runs the plan instead, as mutant does: one fork of the loaded process
#    per mutant, with the mutant switched on, running its candidate tests
#    and stopping at the first that fails.
#
# It requires nothing outside the standard library.

module Supercov
  module Strong
    PLAN_ENV = "SUPERCOV_STRONG_RUBY_PLAN"
    TIMEOUT_CODE = "SUPERCOV_MUTANT_TIMEOUT"

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
        active = ENV["SUPERCOV_ACTIVE_MUTANT"].to_s
        activate(@keys.fetch(active, -1)) unless active.empty? || active == "-"
      end

      def activate(key)
        $__supercov_mutant = key
        @hits = 0
        @timed_out = false
      end

      # One evaluation of the active mutant's replacement.
      def h
        @hits += 1
        return if @hits <= @limit

        unless @timed_out
          @timed_out = true
          note_timeout
        end
        raise MutantTimeout, "#{TIMEOUT_CODE}: the mutant ran past #{@limit} evaluations"
      end

      def timed_out? = @timed_out

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
        @keys.key($__supercov_mutant) || "-"
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

    # The pass: every mutant of the plan, each in a fork of this process.
    class Pass
      # A forked process's work, ended without the parent's `at_exit` hooks
      # (a runner's autorun among them): an error that escapes it ends the
      # process with no outcome written.
      def self.child
        yield
        exit!(0)
      rescue Exception => error # rubocop:disable Lint/RescueException
        warn("[supercov] #{error.class}: #{error.message}")
        exit!(1)
      end

      def initialize(runner)
        @runner = runner
        @plan = Strong.plan
        @jobs = Integer(ENV["SUPERCOV_STRONG_JOBS"] || @plan["jobs"] || 4)
        @results = File.open(@plan["results"], "a")
        @results.sync = true
        @confirmed = {}
      end

      def run
        tests = @runner.tests
        baseline = run_isolated(-1, @plan["baseline"].select { |id| tests.key?(id) }, nil, all: true)
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
        running = {}
        until queue.empty? && running.empty?
          while running.size < @jobs && (mutant = queue.shift)
            start(mutant, tests, running)
          end
          reap(running)
        end
        @results.close
      end

      private

      def candidates(mutant, tests)
        mutant["tests"].select { |id| tests.key?(id) && !@failing[id] }
      end

      def limit(ids)
        total = ids.sum { |id| @durations.fetch(id, 1.0) }
        (total * 3) + 10 + Float(@plan["extraSeconds"] || 0)
      end

      def start(mutant, tests, running)
        ids = candidates(mutant, tests)
        if ids.empty?
          write(mutant, "survived", nil, 0, 0, "no candidate test passes without the mutant")
          return
        end
        reader, writer = IO.pipe
        reader.binmode
        writer.binmode
        started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
        pid = fork do
          reader.close
          Pass.child do
            outcome = attempt(mutant["key"], ids, all: false)
            writer.write(Marshal.dump(outcome))
            writer.close
          end
        end
        writer.close
        # Read as the fork writes: a large outcome would fill the pipe and
        # hold the fork, which would never be reaped.
        output = Thread.new { reader.read }
        running[pid] = { mutant: mutant, reader: reader, output: output, started: started, deadline: started + limit(ids), ids: ids }
      end

      def reap(running)
        loop do
          pid, status = begin
            Process.waitpid2(-1, Process::WNOHANG)
          rescue Errno::ECHILD
            nil
          end
          break unless pid

          job = running.delete(pid)
          next unless job

          finish(job, status)
        end
        now = Process.clock_gettime(Process::CLOCK_MONOTONIC)
        running.each do |pid, job|
          next unless now > job[:deadline]

          Process.kill("KILL", pid) rescue nil # rubocop:disable Style/RescueModifier
          job[:killed] = true
        end
        sleep 0.005
      end

      def finish(job, status)
        mutant = job[:mutant]
        data = job[:output].value
        job[:reader].close
        duration = ((Process.clock_gettime(Process::CLOCK_MONOTONIC) - job[:started]) * 1000).round
        outcome = data.empty? ? nil : Marshal.load(data) # rubocop:disable Security/MarshalLoad
        if outcome.nil?
          status_word = job[:killed] ? "timeout" : "killed"
          reason = job[:killed] ? "its tests ran past their time" : "the test process ended (#{status.inspect}) before its tests did"
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
        if confirmed?(outcome[:sequence])
          write(mutant, "killed", killer, outcome[:ran], duration, nil)
          return
        end
        # The killing test fails without the mutant after the same tests:
        # the candidates' files run whole, with and without it.
        files_whole(mutant, duration)
      end

      # Whether the tests a kill ran, up to the failing one, pass without
      # the mutant.
      def confirmed?(sequence)
        @confirmed.fetch(sequence) do
          baseline = run_isolated(-1, sequence, nil, all: true)
          @confirmed[sequence] = !baseline.nil? && baseline[:failed].empty?
        end
      end

      def files_whole(mutant, duration)
        whole = @runner.whole(mutant["tests"])
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
          until Process.waitpid(pid, Process::WNOHANG)
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
          Process.waitpid(pid)
        end
        data = output.value
        reader.close
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
        status = @runner.run_specs(world.ordered_example_groups)
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

    module RSpecHook
      def run_specs(example_groups)
        return super if Strong.ran || Strong.plan.nil? || ENV["SUPERCOV_STRONG_SWITCHES_ONLY"]

        Strong.ran = true
        Pass.new(RSpecRunner.new(self)).run
        0
      end
    end

    module MinitestHook
      def run(args = [])
        return super if Strong.ran || Strong.plan.nil? || ENV["SUPERCOV_STRONG_SWITCHES_ONLY"]

        Strong.ran = true
        # What `Minitest.run` sets before it runs anything.
        options = respond_to?(:process_args) ? process_args(args) : {}
        self.seed ||= options[:seed] || (Random.new_seed % 0xFFFF)
        srand(seed)
        init_plugins(options) if respond_to?(:init_plugins)
        Pass.new(MinitestRunner.new).run
        true
      end
    end

    def self.install
      path = ENV[PLAN_ENV].to_s
      return if path.empty?

      load_plan(path)
      $__supercov_mutant = -1 if $__supercov_mutant.nil?
      $__supercov_strong = self
      Loader.install
      attach
    end

    # The runners' hooks go on as their classes are defined.
    def self.attach
      TracePoint.new(:end) do |trace|
        case trace.self.name
        when "RSpec::Core::Runner" then ::RSpec::Core::Runner.prepend(RSpecHook)
        when "Minitest" then ::Minitest.singleton_class.prepend(MinitestHook)
        end
      end.enable
      ::RSpec::Core::Runner.prepend(RSpecHook) if defined?(::RSpec::Core::Runner)
      ::Minitest.singleton_class.prepend(MinitestHook) if defined?(::Minitest) && ::Minitest.respond_to?(:run)
    end
  end
end

Supercov::Strong.install
