# frozen_string_literal: true

# Supercov's stdlib-only Ruby runtime.
#
# Rust decides the denominator ahead of the run and ships it as a probe plan.
# This file, loaded through RUBYOPT before any application code, does three
# things and nothing else:
#
# 1. Starts Ruby's own Coverage module and turns its per-phase deltas into
#    first-sighting hits. On Ruby 3.3+ it asks for line events alone: a
#    statement that starts a branch body or method body proves the branch,
#    the method and the decision outcome (the plan's `implied` map), and the
#    rest is probed. Asking for `branches` and `methods` too made every sample
#    rebuild both tables for every loaded file -- 80% of the runtime's cost.
#    Ruby 3.3 takes the same path: it does not apply Coverage to a file
#    compiled through the hook, and every statement there carries a probe.
# 2. Installs a RubyVM::InstructionSequence.load_iseq hook that splices the
#    plan's probe calls into application sources in memory as they load. The
#    files on disk are never touched; no insertion contains a newline.
# 3. Writes commit-framed evidence records that Rust joins into the run.
#
# It never computes a coverage verdict and never requires anything outside
# the standard library.

require "coverage"
# No `require "json"`: this file loads through RUBYOPT before Bundler sets up,
# and activating the json default gem here would clash with an application
# whose Gemfile pins another version. The plan is a Ruby literal and the
# evidence records use the small encoder below.

module Supercov
  PLAN_VERSION = 1
  EVIDENCE_VERSION = 1
  PLAN_ENV = "SUPERCOV_RUBY_PLAN"
  EVIDENCE_DIR_ENV = "SUPERCOV_RUBY_EVIDENCE_DIR"
  RUN_ID_ENV = "SUPERCOV_RUN_ID"
  WORKER_ENV = "SUPERCOV_RUBY_WORKER"
  CONTEXT_ENV = "SUPERCOV_CONTEXT"
  # Set by Supercov's `docker` launcher in a service built from the copy.
  CONTAINER_ENV = "SUPERCOV_CONTAINER"
  SOURCE_ROOT_ENV = "SUPERCOV_RUBY_SOURCE_ROOT"
  DEBUG = !ENV["SUPERCOV_RUBY_DEBUG"].to_s.empty?
  # Escape hatch: a comma-separated list of path fragments to measure through
  # Ruby's Coverage module alone, without probe insertions.
  SKIP_PROBES = ENV["SUPERCOV_RUBY_SKIP_PROBES"].to_s.split(",").map(&:strip).reject(&:empty?)

  TRANSPORT_MAGIC = "SCVRUBY1".b
  # The global Hash statement probes store into (see LoadTime.statement_probes).
  LINE_TABLE = "$__supercov_lines"
  TRANSPORT_VERSION = 1
  TRANSPORT_HEADER_SIZE = 64
  TRANSPORT_RECORD_HEADER_SIZE = 16
  TRANSPORT_INITIAL_CAPACITY = 1024 * 1024
  TRANSPORT_MAX_CAPACITY = 512 * 1024 * 1024
  TRANSPORT_MAX_RECORD_SIZE = 4 * 1024 * 1024
  MAX_OPEN_EVALUATIONS = 64

  # Evidence transport: a preallocated file of framed JSON records. Each frame
  # is [commit u8][3 zero][length u32][checksum u32][4 zero][payload][pad to 8].
  # The commit byte is written last so a killed process never leaves a
  # committed frame without its payload.
  # JSON output and plan input without the json gem.
  module Encode
    module_function

    # The plan file is a Ruby literal written by Supercov itself.
    def load_plan(path)
      plan = Kernel.eval(File.read(path, encoding: "UTF-8"), TOPLEVEL_BINDING.dup, path, 1) # rubocop:disable Security/Eval
      raise "Supercov Ruby plan #{path} is not a Hash" unless plan.is_a?(Hash)

      plan
    end

    ESCAPES = { '"' => '\\"', "\\" => "\\\\", "\n" => "\\n", "\r" => "\\r", "\t" => "\\t", "\b" => "\\b", "\f" => "\\f" }.freeze

    def json(value, out = +"")
      case value
      when nil then out << "null"
      when true then out << "true"
      when false then out << "false"
      when Integer then out << value.to_s
      when Float
        raise ArgumentError, "non-finite float #{value}" unless value.finite?

        out << value.to_s
      when String then string(value, out)
      when Symbol then string(value.to_s, out)
      when Array
        out << "["
        value.each_with_index do |item, index|
          out << "," if index > 0
          json(item, out)
        end
        out << "]"
      when Hash
        out << "{"
        first = true
        value.each do |key, item|
          out << "," unless first
          first = false
          string(key.to_s, out)
          out << ":"
          json(item, out)
        end
        out << "}"
      else
        string(value.to_s, out)
      end
      out
    end

    def string(text, out)
      text = text.encode("UTF-8", invalid: :replace, undef: :replace) unless text.encoding == Encoding::UTF_8 && text.valid_encoding?
      text = text.scrub unless text.valid_encoding?
      out << '"'
      out << text.gsub(/["\\\x00-\x1f]/) { |char| ESCAPES[char] || format("\\u%04x", char.ord) }
      out << '"'
    end
  end

  # Load-time repair of one file's plan, and the only implementation of the
  # column arithmetic the runtime needs. Ruby decides for itself which lines
  # it will ever count, so a statement whose first line this interpreter does
  # not count is given a probe here, and every key on an affected line moves
  # by the same rule the instrumenter applied to the planned insertions.
  # `scripts/ruby-position-sweep.rb` drives these functions over real corpora,
  # which is what keeps that rule honest.
  module LoadTime
    module_function

    def line_starts(source)
      starts = [0]
      source.each_byte.with_index { |byte, index| starts << index + 1 if byte == 10 }
      starts
    end

    # Probes for the plan's statements whose first line `stub` (Ruby's own
    # `Coverage.line_stub`) shows this interpreter will never count. Returns
    # the insertions and the probe targets they need, numbered from `first_key`.
    # `blocked` ranges (Ractor blocks, where no probe may run) get no probe;
    # their statements come back in the third element for the caller to declare.
    #
    # With `table`, every probe is a store into that global Hash rather than a
    # method call -- no frame, no allocation -- and the runtime harvests the
    # table at each phase boundary: the line-level attribution per test that
    # Coverage's oneshot lines cannot give, at the price of one hash store per
    # executed statement.
    #
    # Inside `calls_only` ranges (`refine` blocks, where a refined `Hash#[]=`
    # would answer the store) the method-call probe is used even with `table`.
    #
    # An endless method's body (`def m(...) = super`) follows its `=`
    # directly; a prefix there would end the method before the body, so that
    # probe wraps the body instead (`= (probe; super)`). `source` shows it.
    def statement_probes(receiver, first_key, lines, statement_offsets, stub, blocked = [], table = nil, calls_only = [], source = nil)
      edits = []
      probes = {}
      skipped = []
      key = first_key
      lines.each do |line, id|
        next unless stub[line - 1].nil?

        span = statement_offsets[id]
        next if span.nil?

        if blocked.any? { |start, finish| span[0] >= start && span[0] < finish }
          skipped << id
          next
        end

        probes[key] = { "kind" => "statement", "id" => id }
        refined = calls_only.any? { |start, finish| span[0] >= start && span[0] < finish }
        text = table && !refined ? "#{table}[#{key}] = true; " : "#{receiver}.s(#{key}); "
        before = source && source.byteslice([span[0] - 256, 0].max, [span[0], 256].min)
        if before&.match?(/(?<![=!<>])=\s*\z/)
          edits << { "offset" => span[0], "text" => "(#{text}", "rank" => "opener", "scope" => span[1] }
          edits << { "offset" => span[1], "text" => ")", "rank" => "closer", "scope" => span[0] }
        else
          edits << { "offset" => span[0], "text" => text, "rank" => "statement", "scope" => span[1] }
        end
        key += 1
      end
      [edits, probes, skipped]
    end

    # Planned and load-time insertions in the order they are applied: where
    # both sit at one offset the load-time probe goes first, so the statement
    # is observed before anything wrapping it.
    def merge_edits(planned, extra)
      return planned if extra.empty?

      ranked = extra.map { |edit| [0, edit] } + planned.map { |edit| [1, edit] }
      ranked.each_with_index.sort_by { |(rank, edit), index| [edit["offset"], rank, index] }.map { |(_, edit), _| edit }
    end

    # The insertions grouped by the line they sit on. A key's span only ever
    # moves because of insertions on its own first and last lines, so this is
    # built once per file and every key then looks at two short lists.
    def index_edits(source, edits)
      starts = line_starts(source)
      by_line = {}
      edits.each do |edit|
        line = line_of(starts, edit["offset"])
        (by_line[line] ||= []) << edit
      end
      { starts: starts, by_line: by_line }
    end

    def line_of(starts, offset)
      low = 0
      high = starts.length - 1
      while low < high
        middle = (low + high + 1) / 2
        if starts[middle] <= offset
          low = middle
        else
          high = middle - 1
        end
      end
      low + 1
    end

    # Where a key's span lands once the insertions are in place. Insertions
    # strictly inside the span move what follows them; at the edges the key's
    # kind decides, exactly as in the instrumenter's `shifted`.
    def shift(span, kind, index)
      starts = index[:starts]
      start_line, start_column = span[0]
      end_line, end_column = span[1]
      start_offset = starts[start_line - 1] + start_column
      end_offset = starts[end_line - 1] + end_column
      start_shift = 0
      (index[:by_line][start_line] || []).each do |edit|
        offset = edit["offset"]
        moves = offset < start_offset ||
          (offset == start_offset &&
            if kind == "point"
              true
            elsif edit["rank"] == "closer"
              false
            elsif kind == "list"
              end_offset < edit["scope"]
            elsif edit["rank"] == "opener"
              end_offset <= edit["scope"]
            else
              true
            end)
        start_shift += edit["text"].bytesize if moves
      end
      end_shift = 0
      (index[:by_line][end_line] || []).each do |edit|
        offset = edit["offset"]
        closer = edit["rank"] == "closer"
        moves = offset < end_offset ||
          (offset == end_offset &&
            if kind == "point"
              true
            elsif closer && kind == "list"
              edit["scope"] >= start_offset
            elsif closer
              edit["scope"] > start_offset
            else
              false
            end)
        end_shift += edit["text"].bytesize if moves
      end
      [[start_line, start_column + start_shift], [end_line, end_column + end_shift]]
    end

    # Sources are read as bytes; Ruby's default source encoding is UTF-8 and a
    # magic comment in the file still overrides it when compiling.
    # The result is UTF-8 whatever the bytes were read as, which is what Ruby
    # assumes for a file it compiles itself; a magic comment in the source
    # still wins. Returning the binary string for a file with no insertions
    # made its literals and regexps ASCII-8BIT under measurement.
    def apply_edits(source, edits)
      return source.dup.force_encoding(Encoding::UTF_8) if edits.empty?

      pieces = []
      cursor = 0
      edits.each do |edit|
        offset = edit["offset"]
        pieces << source.byteslice(cursor, offset - cursor)
        pieces << edit["text"].b
        cursor = offset
      end
      pieces << source.byteslice(cursor, source.bytesize - cursor)
      pieces.join.force_encoding(Encoding::UTF_8)
    end
  end

  class Transport
    attr_reader :path

    def initialize(directory, worker, pid)
      Dir.mkdir(directory) unless Dir.exist?(directory)
      safe_worker = worker.gsub(/[^A-Za-z0-9._-]/, "_")
      token = format("%x-%x", Process.clock_gettime(Process::CLOCK_REALTIME, :nanosecond), object_id & 0xFFFF)
      @path = File.join(directory, "#{safe_worker}.#{pid}.#{token}.mmap")
      # Readable outside a container, whose user (root) owns it there.
      @file = File.open(@path, File::RDWR | File::CREAT | File::EXCL, ENV[CONTAINER_ENV] == "1" ? 0o644 : 0o600)
      @file.binmode
      @file.sync = true
      @capacity = TRANSPORT_INITIAL_CAPACITY
      @file.truncate(@capacity)
      @cursor = TRANSPORT_HEADER_SIZE
      @dropped = 0
      @lock = Mutex.new
      header = [TRANSPORT_MAGIC, TRANSPORT_VERSION, TRANSPORT_HEADER_SIZE, @capacity, 0, pid].pack("a8L<L<Q<Q<Q<")
      @file.pwrite(header.ljust(TRANSPORT_HEADER_SIZE, "\0"), 0)
    end

    # Probes fire from whichever thread runs the test, so frame allocation and
    # the two writes happen under one lock; a torn frame would otherwise be
    # read back as corruption.
    def write(record)
      payload = Encode.json(record).b
      @lock.synchronize do
        if payload.bytesize > TRANSPORT_MAX_RECORD_SIZE
          drop
          return
        end
        payload_end = @cursor + TRANSPORT_RECORD_HEADER_SIZE + payload.bytesize
        next_cursor = (payload_end + 7) & ~7
        if next_cursor > @capacity && !grow(next_cursor)
          drop
          return
        end
        frame = [0, 0, 0, 0, payload.bytesize, checksum(payload), 0].pack("CCCCL<L<L<")
        @file.pwrite(frame + payload + ("\0" * (next_cursor - payload_end)), @cursor)
        @file.pwrite("\x01".b, @cursor)
        @cursor = next_cursor
      end
    end

    def close
      @lock.synchronize { @file.close unless @file.closed? }
    end

    private

    def checksum(payload)
      value = 0x811C9DC5
      payload.each_byte do |byte|
        value ^= byte
        value = (value * 0x01000193) & 0xFFFFFFFF
      end
      value
    end

    def grow(required)
      capacity = @capacity
      capacity = [capacity * 2, TRANSPORT_MAX_CAPACITY].min while capacity < required && capacity < TRANSPORT_MAX_CAPACITY
      return false if capacity < required

      @file.truncate(capacity)
      @capacity = capacity
      @file.pwrite([capacity].pack("Q<"), 16)
      true
    end

    def drop
      @dropped += 1
      @file.pwrite([@dropped].pack("Q<"), 24)
    end
  end

  class Runtime
    attr_reader :plan, :root, :worker, :closed
    attr_accessor :adapter_active

    def initialize(plan_path, evidence_dir, run_id, worker)
      @plan = Encode.load_plan(plan_path)
      raise "unsupported Supercov Ruby plan version #{@plan['version'].inspect}" unless @plan["version"] == PLAN_VERSION

      @root = File.realpath(@plan["root"])
      # A server a browser suite starts runs in Supercov's copy of the
      # project, `root`, and may still load the project from where it is: a
      # Gemfile `path:` gem or an absolute `require`.
      source_root = ENV[SOURCE_ROOT_ENV].to_s
      @source_root = source_root.empty? ? nil : File.realpath(source_root)
      @files = @plan["files"]
      # In an image built from the copy (Supercov's `docker` launcher says
      # so), the copy's files are at a path of the image's own (`/app`),
      # found from the first file loaded from it (`container_relative`).
      @container_map = nil
      @container_tried = {}
      @in_container = ENV[CONTAINER_ENV] == "1"
      @probes = {}
      @plan["probes"].each { |key, target| @probes[Integer(key)] = target }
      @receiver = @plan["receiver"] || "$__supercov"
      # Keys for probes synthesized at load time never collide with the plan's.
      @dynamic_key = 1 << 40
      @evidence_dir = evidence_dir
      @run_id = run_id
      @worker = worker
      @adapter_active = false
      @closed = false
      @mutex = Mutex.new
      @seen_lock = Mutex.new
      @transport_lock = Mutex.new
      @transport = nil
      @transport_pid = nil
      @context = 0
      @next_context = 1
      @identities = {}
      @seen_hits = {}
      @compiled = {}
      @unprobed_lines = {}
      @features_seen = 0
      @pending_hits = {}
      @id_numbers = {}
      @line_ids = {}
      @seen_vectors = {}
      @vector_counts = {}
      @open = {}
      @loop_state = {}
      @pending_loops = {}
      # Guards adding to and settling the loops and arrivals a phase leaves
      # open. Probes on a thread with no phase of its own add them while
      # another thread's phase ends; adding to a hash being iterated raised,
      # and a copy taken first was not atomic on Ruby 4.0, which then
      # crashed. Taken once per loop or arrival a phase opens, never per
      # iteration.
      @settle_lock = Mutex.new
      @arrivals = {}
      @limitations = {}
      @active_threads = {}
      @asserted = {}
      @assertion_hooks = {}
      @realpath_cache = {}
      @saw_file = false
      @matched_file = false
      # Every statement of an instrumented file carries a probe, so a file
      # compiled through the load hook needs nothing from Coverage -- which
      # 3.3 does not apply to such files. Older Rubies run on stdlib coverage
      # alone and declare every probe-only obligation unmeasured.
      @probes_supported = Supercov.probes_supported?
      # Coverage's branch and method keys are read only where probes cannot
      # stand in for them.
      @stdlib_keys = !@probes_supported
      @stdlib_every_phase = @stdlib_keys
      compile_file_plans
    end

    def probes_supported? = @probes_supported

    # Ruby's own line table decides which lines can ever be counted. A plan
    # statement whose first line is not countable on this interpreter (the
    # `case ... in` line on 3.3, for example) is declared unmeasured instead
    # of appearing as a gap the tests could never close.
    def declare_uncountable_lines
      @lines_by_file.each_key { |absolute| declare_uncountable_lines_for(absolute) }
    end

    def declare_uncountable_lines_for(absolute)
      lines = @lines_by_file[absolute]
      return if lines.nil? || !File.file?(absolute)

      stub = begin
        Coverage.line_stub(absolute)
      rescue StandardError
        return
      end
      lines.each do |line, id|
        next unless stub[line - 1].nil?

        limitation(
          "ruby-line-not-countable",
          "Ruby #{RUBY_VERSION} records no line event for this statement's first line, so it cannot be observed on this interpreter",
          relative(absolute),
          id,
        )
      end
    end

    def declare_probe_gap(obligations)
      obligations.each do |id|
        limitation(
          "ruby-probe-obligations-need-3.4",
          "Ruby #{RUBY_VERSION} does not measure code compiled by a load hook, so obligations that need a probe (multi-condition decisions, ||=, loops, rescue flow, same-line statements) are unmeasured; Ruby 3.4 or newer measures them",
          nil,
          id,
        )
      end
    end

    # -- plan compilation ---------------------------------------------------

    def compile_file_plans
      @lines_by_file = {}
      @statement_offsets_by_file = {}
      @branch_keys_by_file = {}
      @method_keys_by_file = {}
      @cases_by_file = {}
      @edits_by_file = {}
      @span_field_by_file = {}
      @implied = {}
      @files.each do |relative, file_plan|
        absolute = File.join(@root, relative)
        @lines_by_file[absolute] = file_plan["lines"].transform_keys(&:to_i)
        @statement_offsets_by_file[absolute] = file_plan["statementOffsets"] || {}
        @cases_by_file[absolute] = file_plan["cases"]
        @edits_by_file[absolute] = file_plan["edits"]
        (file_plan["implied"] || {}).each { |id, plan| @implied[id] = plan }
        index_file_keys(absolute, file_plan, @probes_supported ? "span" : "unshifted")
      end
    end

    # Positions the runtime will look for in Ruby's results: the shifted ones
    # while this file carries its insertions, the source's own otherwise.
    def index_file_keys(absolute, file_plan, span_field)
      @span_field_by_file[absolute] = span_field
      keys = {}
      file_plan["branches"].each do |branch|
        key = branch["key"]
        keys[[key["group"], key["branch"], *flatten_span(key[span_field])]] = branch
      end
      @branch_keys_by_file[absolute] = keys
      @method_keys_by_file[absolute] = file_plan["methods"].to_h { |method| [flatten_span(method[span_field]), method["id"]] }
    end

    def flatten_span(span)
      [span[0][0], span[0][1], span[1][0], span[1][1]]
    end

    # -- source transformation ----------------------------------------------

    # Called by the load_iseq hook for every file Ruby is about to compile.
    # Returns nil for files outside the plan so the default loader runs.
    def compile(path)
      return nil unless @probes_supported

      absolute = realpath(path)
      edits = absolute && @edits_by_file[absolute]
      # In an image of the copy, a file is the copy's by its path there.
      if edits.nil? && @in_container && absolute && (inside = relative(absolute))
        absolute = File.join(@root, inside)
        edits = @edits_by_file[absolute]
      end
      return nil if edits.nil?

      if SKIP_PROBES.any? { |fragment| absolute.include?(fragment) }
        uninstrumented(absolute, "SUPERCOV_RUBY_SKIP_PROBES asked for this file to be measured through Ruby's Coverage module alone")
        return nil
      end

      source = File.binread(path)
      edits = probe_uncountable_lines(absolute, source, edits)
      # Nothing to insert: Ruby's own loader compiles the untouched file, with
      # whatever cache (bootsnap) it keeps, exactly as without Supercov.
      return nil if edits.empty?

      transformed = LoadTime.apply_edits(source, edits)
      iseq = RubyVM::InstructionSequence.compile(transformed, path, path, 1)
      @compiled[absolute] = true
      iseq
    rescue SyntaxError, StandardError => error
      # Measuring must never break the program: this file loads unmodified,
      # and everything only a probe could have proven there is declared.
      uninstrumented(
        absolute,
        "Supercov could not compile this file with its probes (#{error.class}: #{error.message.to_s.lines.first.to_s.strip}), so it was measured through Ruby's Coverage module alone",
      )
      nil
    end

    # One file carries no insertions, because compiling it with them failed
    # or because the run asked for it to be left alone. Ruby loads and
    # measures the untouched source instead, so its keys revert to the
    # positions in that source and its probe obligations are declared.
    def uninstrumented(absolute, reason)
      return if absolute.nil? || @edits_by_file.delete(absolute).nil?

      # Its lines are Coverage's to report now, phase by phase, and a one-shot
      # line goes to the first test that runs it.
      @stdlib_every_phase = true
      record("t" => "lines", "mode" => "first")

      file_plan = @files[relative(absolute)]
      return if file_plan.nil?

      index_file_keys(absolute, file_plan, "unshifted")
      # Only what a probe would have proven is declared: Ruby still applies
      # Coverage to the untouched source, so lines, methods and the branches
      # it reports itself stay measured and keep their place in the total.
      debug("#{relative(absolute)}: #{reason}")
      (file_plan["probeObligations"] || []).each do |id|
        limitation("ruby-file-not-instrumented", reason, relative(absolute), id)
      end
      # Ruby 3.4+ reads no branch or method keys, so what only a key proved
      # in this file, and no line-owned statement implies, is declared too.
      if @probes_supported
        key_only_obligations(file_plan, absolute).each do |id|
          limitation("ruby-file-not-instrumented", reason, relative(absolute), id)
        end
      end
      declare_uncountable_lines_for(absolute)
    end

    def key_only_obligations(file_plan, absolute)
      keyed = []
      file_plan["branches"].each do |branch|
        keyed.concat(branch["hits"])
        if (decision = branch["decision"])
          keyed << decision["id"] << decision["outcome"]
        end
      end
      file_plan["cases"].each do |case_plan|
        case_plan["clauses"].each { |clause| keyed << clause["missed"] << clause["selected"] }
        if (no_match = case_plan["noMatch"])
          keyed << no_match["matched"] << no_match["unmatched"]
        end
      end
      file_plan["methods"].each { |method| keyed << method["id"] }
      # Implications whose statement Ruby's own line table observes still
      # hold in an untouched file; those behind a probe do not, and nor do
      # those of a statement on a line this interpreter never counts (3.3
      # has no line event for `case ... in`, `x = begin` and the like).
      stub = begin
        Coverage.line_stub(absolute)
      rescue StandardError
        []
      end
      line_owned = file_plan["lines"].filter_map { |line, id| id unless stub[line.to_i - 1].nil? }
      implied = (file_plan["implied"] || {}).flat_map do |id, plan|
        next [] unless line_owned.include?(id)

        (plan["hits"] || []) + (plan["decisions"] || []).flat_map { |decision| [decision["id"], decision["outcome"]] }
      end
      (keyed - implied).map { |id| obligation_of(id) }.uniq
    end

    # A point is its own obligation; an alternative belongs to its branch.
    def obligation_of(id)
      id.count(":") >= 3 ? id.rpartition(":").first : id
    end

    # Ruby's own line table decides which lines can be counted. A statement
    # the plan expects to prove by its first line, on a line this interpreter
    # never counts (`begin`, a `case` without subject, a multi-line literal),
    # gets a statement probe here instead, and the stdlib keys on those lines
    # are re-shifted with the same rule Rust applied to the planned edits.
    def probe_uncountable_lines(absolute, source, edits)
      lines = @lines_by_file[absolute]
      offsets = @statement_offsets_by_file[absolute]
      return edits if lines.nil? || lines.empty? || offsets.nil?

      # Every statement gets a table probe: Coverage's oneshot lines fire once
      # per process, which credits a line to the first test that ran it and
      # to no other, and a test's line set is what test impact and assertion
      # coverage read.
      stub = []
      file_plan = @files[relative(absolute)]
      blocked = file_plan["ractorBlocks"] || []
      refined = file_plan["refineBlocks"] || []
      # The line table is a global, which a Ractor cannot store into.
      table = @plan["ractors"] ? nil : LINE_TABLE
      extra, probes, skipped = LoadTime.statement_probes(@receiver, @dynamic_key, lines, offsets, stub, blocked, table, refined, source)
      unless skipped.empty?
        # A statement in a Ractor block takes no probe: Coverage's one-shot
        # line reports it, phase by phase, to the first test that runs it.
        by_id = lines.to_h { |line, id| [id, line] }
        @unprobed_lines[absolute] = skipped.filter_map { |id| by_id[id] }.to_h { |line| [line, true] }
        @stdlib_every_phase = true
        record("t" => "lines", "mode" => "first")
      end
      skipped.each do |id|
        limitation(
          "ruby-line-not-countable",
          "Ruby #{RUBY_VERSION} records no line event for this statement's first line, and inside a Ractor block no probe can stand in for it",
          relative(absolute),
          id,
        )
      end
      return edits if extra.empty?

      @probes.merge!(probes)
      probes.each { |key, target| @line_ids[key] = target["id"] }
      @dynamic_key += probes.size
      merged = LoadTime.merge_edits(edits, extra)
      reshift_keys(absolute, source, merged)
      merged
    end

    # The plan's keys are positions in the source Rust transformed; the
    # load-time probes move some of them again.
    def reshift_keys(absolute, source, edits)
      index = LoadTime.index_edits(source, edits)
      file_plan = @files[relative(absolute)]
      file_plan["branches"].each do |branch|
        key = branch["key"]
        key["span"] = LoadTime.shift(key["unshifted"], key["kind"], index)
      end
      file_plan["cases"].each do |case_plan|
        case_plan["clauses"].each do |clause|
          clause["key"]["span"] = LoadTime.shift(clause["key"]["unshifted"], clause["key"]["kind"], index)
        end
        no_match = case_plan["noMatch"]
        no_match["key"]["span"] = LoadTime.shift(no_match["key"]["unshifted"], no_match["key"]["kind"], index) if no_match
      end
      file_plan["methods"].each do |method|
        method["span"] = LoadTime.shift(method["unshifted"], "node", index)
      end
      index_file_keys(absolute, file_plan, "span")
    end

    def realpath(path)
      cached = @realpath_cache[path]
      return cached unless cached.nil?

      resolved = begin
        File.realpath(path)
      rescue SystemCallError
        false
      end
      @realpath_cache[path] = resolved
      resolved
    end

    # `absolute` relative to the project when it is a file the plan measures.
    def measured_file(absolute)
      file = relative(absolute)
      file if file && @files.key?(file)
    end

    def relative(absolute)
      return nil unless absolute

      @saw_file = true
      root = [@root, @source_root].compact.find { |candidate| absolute.start_with?(candidate + "/") }
      if root.nil? && @in_container
        return @container_map ? mapped_relative(absolute) : container_relative(absolute)
      end
      return nil unless root

      relative = absolute[(root.length + 1)..]
      # A file of the copy under the checkout is the copy's, not a path of
      # the checkout's own.
      return nil unless root == @root || @files.key?(relative)

      @matched_file = true
      relative
    end

    # A Dockerfile copies the copy's files to a path of its own, and may move
    # them (`COPY backend/lib/ ./lib/`): an image file and a planned file that
    # end alike, a directory and the name at least, give the image's prefix
    # for the copy's. The pair is taken only if at least three of the planned
    # files under that prefix (all, when fewer) are at their places in the
    # image too: one generic name, a gem's `version.rb`, is never taken for
    # the project's.
    def container_relative(absolute)
      parts = absolute.split("/")
      @files.each_key do |relative|
        planned = relative.split("/")
        next unless planned.last == parts.last

        shared = 0
        shared += 1 while shared < [parts.size, planned.size].min && parts[-1 - shared] == planned[-1 - shared]
        next if shared < 2

        image_prefix = parts[0...-shared].join("/")
        copy_prefix = planned[0...-shared].join("/")
        next if @container_tried[[image_prefix, copy_prefix]]

        @container_tried[[image_prefix, copy_prefix]] = true
        under = @files.keys.select { |name| copy_prefix.empty? || name.start_with?(copy_prefix + "/") }.first(64)
        need = [3, under.size].min
        next if need.zero?
        next unless under.count { |name| File.file?(File.join(image_prefix, name.delete_prefix(copy_prefix).delete_prefix("/"))) } >= need

        @container_map = [image_prefix, copy_prefix]
        @matched_file = true
        return relative
      end
      nil
    end

    def mapped_relative(absolute)
      image_prefix, copy_prefix = @container_map
      return nil unless absolute.start_with?(image_prefix + "/")

      rest = absolute[(image_prefix.length + 1)..]
      relative = copy_prefix.empty? ? rest : "#{copy_prefix}/#{rest}"
      return nil unless @files.key?(relative)

      @matched_file = true
      relative
    end

    # -- identity -----------------------------------------------------------

    # The phase a probe attributes to: the thread's own phase when a runner
    # drives tests on several threads, else the process-wide current phase
    # (threads a test spawns inherit that).
    def current_context
      Thread.current[:__supercov_context] || @context
    end

    # Enter a test phase (nil for background). Flushes the stdlib coverage
    # delta accumulated for the phase that ends and starts a fresh window.
    # Stdlib counters are process-wide, so a delta collected while another
    # thread is mid-phase belongs to no single test: it goes to the run's
    # background and the run says so. Probe hits stay exact per thread.
    def switch(identity)
      @mutex.synchronize do
        thread = Thread.current
        ending = thread[:__supercov_context] || @context
        settle_arrivals(ending)
        settle_loops(ending)
        sample_stdlib(thread, ending)
        if identity.nil?
          @context = 0
          thread[:__supercov_context] = nil
          @active_threads.delete(thread)
        else
          context = @next_context
          @next_context += 1
          stored = {
            "worker" => identity[:worker] || @worker,
            "test" => identity[:test],
            "retry" => identity[:retry].to_i,
            "phase" => identity[:phase],
          }
          @identities[context] = stored
          # Declared before any thread can read it: a thread with no phase of
          # its own reads the process-wide one without this lock, and one that
          # read it first recorded under a context the evidence had not
          # declared yet (Campfire's system tests, a Puma request beside an
          # Action Cable thread), and the run could not be published.
          record({ "t" => "phase", "ctx" => context, "at" => now_ms }.merge(stored))
          @context = context
          thread[:__supercov_context] = context
          @active_threads[thread] = context
          hook_assertions if identity[:phase] == "call"
        end
        @context
      end
    end

    # Flush the stdlib coverage delta into `context`. Stdlib counters are
    # process-wide, so a delta collected while another thread is mid-phase
    # belongs to no single test: it goes to the run's background and the run
    # says so. Probe hits stay exact per thread.
    def sample_stdlib(thread, context)
      others = @active_threads.filter_map { |other, ctx| ctx if other != thread && ctx != 0 && other.alive? }
      if !others.empty? && !one_attempt?([context, *others])
        limitation(
          "ruby-concurrent-test-phases",
          "tests ran concurrently in threads; line, branch and method observations made while phases overlapped are attributed to the run, not to a test (probe observations stay exact)",
        )
        collect_stdlib(0)
      else
        collect_stdlib(context)
      end
    end

    # Whether phases are of one test attempt at most, counting a request that
    # named no test as no attempt's: a suite's own test and the requests it
    # makes to a server the suite runs in a thread, which the report gives to
    # that test by time. Their lines are that test's either way.
    def one_attempt?(contexts)
      contexts.filter_map do |ctx|
        identity = @identities[ctx]
        [identity["worker"], identity["test"], identity["retry"]] if identity && identity["worker"] != Requests::REQUEST_WORKER
      end.uniq.size <= 1
    end

    # The first assertion of a call phase. The stdlib delta the phase has
    # accumulated is sampled into it, then the marker says everything the
    # phase recorded so far ran before an assertion; the report links it to
    # that assertion once the phase passes. Sampling once keeps the cost to
    # one `Coverage.result` per test: later assertions cost a hash lookup.
    def assertion
      context = current_context
      return if context.zero? || @asserted[context]

      @mutex.synchronize do
        return if @asserted[context]

        identity = @identities[context]
        return if identity.nil? || identity["phase"] != "call"

        @asserted[context] = true
        sample_stdlib(Thread.current, context)
        record("t" => "assert", "ctx" => context)
      end
    end

    # Where in the test an assertion ran, so an assertion map can tell one
    # site from another. Ruby backtraces carry no column, so this reports the
    # file and line only; the report resolves the column against the syntax
    # inventory Supercov captured before the run, and a frame that is not an
    # inventoried site matches nothing rather than inventing a witness.
    #
    # Recorded once per site per test: the first sighting pays for the caller
    # scan and one write, later ones a hash lookup. Only the call phase is
    # reported, because setup and teardown assertions witness no test.
    #
    # Each assertion library reaches the runtime through one method: every
    # Minitest assert_*/refute_* ends in `assert`, every RSpec expectation in
    # `to`/`not_to`, every test-unit assertion runs inside `_wrap_assertion`.
    # Prepending to the module reaches the classes that already include it.
    ASSERTION_METHODS = {
      "Minitest::Assertions" => %i[assert],
      "RSpec::Expectations::ExpectationTarget" => %i[to not_to to_not],
      "Test::Unit::Assertions" => %i[_wrap_assertion],
    }.freeze

    # Hooked when a call phase starts rather than at install, so a library
    # loaded late -- Cucumber pulls rspec-expectations in with its World --
    # is still caught; a hooked library costs one hash lookup per phase.
    def hook_assertions
      return if @assertion_hooks.size == ASSERTION_METHODS.size

      ASSERTION_METHODS.each do |name, methods|
        next if @assertion_hooks[name] || !Object.const_defined?(name)

        owner = Object.const_get(name)
        runtime = self
        hook = Module.new do
          methods.each do |method_name|
            define_method(method_name) do |*args, **options, &block|
              runtime.assertion
              begin
                super(*args, **options, &block)
              rescue Exception => error # rubocop:disable Lint/RescueException
                runtime.without_own_frames(error)
                raise
              end
            end
          end
        end
        owner.prepend(hook)
        @assertion_hooks[name] = true
      end
    end

    # A failed assertion passes through the method hooked above, so its
    # backtrace held a frame of this file between the library's and the
    # test's. Minitest names a failure by the first frame that is not its
    # own: `CryptoTest#test_compares [.supercov/…/supercov_runtime.rb:818]`
    # for `[test/crypto_test.rb:15]`. The frame is taken out before anything
    # reads the backtrace, with any other frame of the runtime's own
    # directory. Ruby 3.4 keeps the locations when given them.
    def without_own_frames(error)
      own = "#{File.dirname(File.expand_path(__FILE__))}/"
      locations = error.backtrace_locations
      if locations && RUBY_VERSION >= "3.4"
        # A method defined from text has a path and no absolute path.
        kept = locations.reject { |location| (location.absolute_path || location.path).to_s.start_with?(own) }
        error.set_backtrace(kept) if kept.size != locations.size
        return
      end
      frames = error.backtrace
      return if frames.nil?

      kept = frames.reject { |frame| frame.start_with?(own) }
      error.set_backtrace(kept) if kept.size != frames.size
    rescue StandardError
      nil
    end

    # The identity of the phase this thread runs in, or nil for background.
    def current_identity
      @identities[current_context]
    end

    # The phase this thread entered itself, else the one the process was
    # started in by a test (SUPERCOV_CONTEXT). Not another thread's: a
    # server's request threads share the process-wide phase, and a request
    # read as the test another thread was serving ran as no one's once that
    # thread left it.
    attr_accessor :inherited_identity

    def own_identity
      context = Thread.current[:__supercov_context]
      context ? @identities[context] : @inherited_identity
    end

    def child_environment
      identity = @identities[current_context]
      return {} if identity.nil?

      { CONTEXT_ENV => [Marshal.dump(identity)].pack("m0") }
    end

    # `file` is where the runner says the test is defined. Assertion maps
    # select tests by source file and name, and a runner identity alone
    # ("CalcTest#test_x", "spec/m_spec.rb[1:1]") is not a path. Adapters that
    # cannot name the file leave it nil and the report falls back to deriving
    # one from the identity.
    def outcome(worker, test, retry_index, phase, outcome, xfail, runner, file = nil, tags = nil, title = nil)
      @mutex.synchronize do
        entry = {
          "t" => "outcome",
          "worker" => worker,
          "test" => test,
          "retry" => retry_index.to_i,
          "phase" => phase,
          "outcome" => outcome,
          "xfail" => xfail ? true : false,
          "runner" => runner,
        }
        entry["file"] = file if file
        entry["tags"] = tags if tags && !tags.empty?
        entry["title"] = title if title.is_a?(String) && !title.empty?
        record(entry)
      end
    end

    # -- stdlib coverage deltas ---------------------------------------------

    def collect_stdlib(context, final: false)
      collect_observations(context, final)
      flush_hits
    end

    def collect_observations(context, final)
      table = $__supercov_lines
      harvest_lines(context, table.keys) unless table.empty?
      table.clear
      # Ruby 3.4+ probes every statement of an instrumented file, so reading
      # Coverage -- a hash of every file it tracks, gems included, at every
      # phase boundary -- adds nothing for them. It is read per phase only
      # once a measured file has been loaded without probes, and always at
      # exit for whatever remains.
      return unless final || @stdlib_every_phase || features_without_probes?

      result = Coverage.result(stop: false, clear: true)
      result.each do |path, data|
        lines = @lines_by_file[path]
        # A file compiled with probes has every statement probed, test by
        # test; its oneshot lines would only credit the run with them again.
        next if lines.nil?

        unprobed = @compiled[path] && (@unprobed_lines[path] || {})
        next if unprobed && unprobed.empty?

        executed = data[:oneshot_lines] || []
        executed.each do |line|
          next if unprobed && !unprobed[line]

          id = lines[line]
          hit(context, id) if id
        end
        next unless @stdlib_keys

        keys = @branch_keys_by_file[path]
        selected = {}
        (data[:branches] || {}).each do |group, branches|
          group_type = group[0].to_s
          branches.each do |branch, count|
            next if count.zero?

            key = [group_type, branch[0].to_s, branch[2], branch[3], branch[4], branch[5]]
            selected[key] = count
            plan = keys[key]
            if plan.nil?
              debug("unmatched stdlib branch key #{key.inspect} in #{path}")
              next
            end
            plan["hits"].each { |id| hit(context, id) }
            if (decision = plan["decision"])
              vector(context, decision["id"], decision["value"] ? "2" : "1", decision["value"])
              hit(context, decision["outcome"])
            end
          end
        end
        derive_cases(context, @cases_by_file[path], selected, @span_field_by_file[path])
        methods = @method_keys_by_file[path]
        (data[:methods] || {}).each do |method, count|
          next if count.zero?

          id = methods[[method[2], method[3], method[4], method[5]]]
          hit(context, id) if id
        end
      end
    end

    # A when/in clause was missed in every execution that selected a later
    # clause or fell through to the implicit else; an explicit else was
    # missed in every execution that selected an earlier clause. Counts, not
    # flags: one phase can hold both kinds of execution.
    def derive_cases(context, cases, selected, span_field)
      cases.each do |case_plan|
        clauses = case_plan["clauses"]
        no_match = case_plan["noMatch"]
        counts = clauses.map { |clause| selected[key_of(clause["key"], span_field)] || 0 }
        implicit = no_match ? (selected[key_of(no_match["key"], span_field)] || 0) : 0
        if no_match
          hit(context, no_match["unmatched"]) if implicit.positive?
          hit(context, no_match["matched"]) if counts.sum.positive?
        end
        clauses.each_with_index do |clause, index|
          later = counts[(index + 1)..].sum + implicit
          earlier = counts[0...index].sum
          explicit_else = clause["key"]["branch"] == "else"
          missed = explicit_else ? earlier.positive? : later.positive?
          hit(context, clause["missed"]) if missed
        end
      end
    end

    def key_of(key, span_field)
      [key["group"], key["branch"], *flatten_span(key[span_field])]
    end

    # -- observations -------------------------------------------------------

    # First sightings are queued per context and written at the next
    # boundary as one record (see flush_hits): a test reaches hundreds of
    # statements, and a record apiece cost more than the tests themselves.
    def hit(context, id)
      seen = @seen_hits[context] ||= {}
      return if seen[id]

      @seen_lock.synchronize do
        return if seen[id]

        seen[id] = true
        (@pending_hits[context] ||= []) << id
      end
      imply(context, id)
    end

    # The statements a phase's probes stored, as first sightings: one lock
    # for the batch rather than one per statement.
    def harvest_lines(context, keys)
      seen = @seen_hits[context] ||= {}
      fresh = nil
      @seen_lock.synchronize do
        keys.each do |key|
          id = @line_ids[key]
          next if id.nil? || seen[id]

          seen[id] = true
          (fresh ||= []) << id
        end
        (@pending_hits[context] ||= []).concat(fresh) if fresh
      end
      fresh&.each { |id| imply(context, id) }
    end

    # Whether a measured file was required without passing through the
    # probe compiler (loaded before the hook, or by a loader that bypasses
    # it). Checked only when the loaded-features list has grown.
    def features_without_probes?
      features = $LOADED_FEATURES
      return false if features.size == @features_seen

      fresh = features[@features_seen..] || []
      @features_seen = features.size
      fresh.each do |feature|
        absolute = realpath(feature)
        next unless absolute && @lines_by_file.key?(absolute) && !@compiled[absolute]

        @stdlib_every_phase = true
      end
      @stdlib_every_phase
    end

    # The queued hits, one record per context; each id is named once per
    # process and numbered after that.
    def flush_hits
      pending = @seen_lock.synchronize do
        taken = @pending_hits
        @pending_hits = {}
        taken
      end
      return if pending.empty?

      # Open (or, in a forked child, reopen) the evidence file first: that
      # decides which numbers the file has already named.
      ensure_transport
      pending.each do |context, ids|
        fresh = []
        numbers = ids.map do |id|
          @id_numbers[id] ||= begin
            fresh << id
            @id_numbers.size
          end
        end
        record("t" => "ids", "first" => @id_numbers.size - fresh.size, "names" => fresh) unless fresh.empty?
        record("t" => "hits", "ctx" => context, "n" => numbers)
      end
    end

    # What one observation proves besides itself: the branch alternatives and
    # the method whose body starts with this statement, and the
    # single-condition decision outcome it decides. One hash lookup per first
    # sighting; nothing per execution.
    def imply(context, id)
      implied = @implied[id]
      return if implied.nil?

      (implied["hits"] || []).each { |other| hit(context, other) }
      (implied["decisions"] || []).each do |decision|
        vector(context, decision["id"], decision["value"] ? "2" : "1", decision["value"])
        hit(context, decision["outcome"])
      end
    end

    def vector(context, decision_id, digits, outcome)
      key = [context, decision_id, digits]
      return if @seen_vectors[key]

      @seen_lock.synchronize do
        return if @seen_vectors[key]

        @seen_vectors[key] = true
        count_key = [context, decision_id]
        @vector_counts[count_key] = (@vector_counts[count_key] || 0) + 1
      end
      record("t" => "dec", "ctx" => context, "id" => decision_id, "v" => digits, "o" => outcome ? 1 : 0)
    end

    def limitation(id, reason, file = nil, obligation = nil)
      key = [id, file, obligation]
      return if @limitations[key]

      @limitations[key] = true
      entry = { "t" => "limitation", "id" => id, "reason" => reason }
      entry["file"] = file if file
      entry["obligation"] = obligation if obligation
      record(entry)
    end

    # -- probe callbacks (the receiver bound to $__supercov) ----------------

    # Condition leaf: records the operand's truthiness and passes it through.
    def probe_condition(key, index, value)
      target = @probes[key]
      frame_key = [current_context, key]
      stack = (@open[frame_key] ||= [])
      last = stack.last
      if last.nil? || index <= last[:last]
        stack.shift if stack.length >= MAX_OPEN_EVALUATIONS
        last = { values: Array.new(target["width"]), last: -1 }
        stack << last
      end
      last[:last] = index
      last[:values][index] = value ? true : false
      value
    end

    # Decision outcome: closes the innermost open evaluation.
    def probe_decision(key, value)
      target = @probes[key]
      frame_key = [current_context, key]
      stack = @open[frame_key]
      frame = stack && stack.pop
      truthy = value ? true : false
      values = if frame
                 frame[:values]
               elsif target["width"] == 1
                 # A lone condition carries no c() wrapper: its value is the
                 # outcome, read back through the condition's own negation.
                 [target["not"][0] ? !truthy : truthy]
               else
                 Array.new(target["width"])
               end
      finish_decision(target, values, truthy)
      value
    end

    def finish_decision(target, values, outcome)
      values = values.map { |v| v.nil? ? nil : v }
      target["not"].each_with_index do |negated, index|
        values[index] = !values[index] if negated && !values[index].nil?
      end
      expected = evaluate_tree(target["tree"], values)
      if expected.nil? || expected != outcome
        limitation("ruby-decision-vector-inconsistent", "observed condition values do not evaluate to the observed outcome", nil, target["id"])
        return
      end
      digits = values.map { |v| v.nil? ? "0" : (v ? "2" : "1") }.join
      vector(current_context, target["id"], digits, outcome)
      hit(current_context, outcome ? target["outcomeTrue"] : target["outcomeFalse"])
      target["logical"].each do |logical|
        previous = logical["previousLeaves"].any? { |i| !values[i].nil? }
        operand = logical["operandLeaves"].any? { |i| !values[i].nil? }
        if operand
          hit(current_context, logical["evaluated"])
        elsif previous
          hit(current_context, logical["shortCircuit"])
        end
      end
    end

    def evaluate_tree(tree, values)
      return values[tree] if tree.is_a?(Integer)

      op = tree["op"]
      result = nil
      tree["items"].each do |item|
        result = evaluate_tree(item, values)
        return nil if result.nil?
        break if (op == "and" && result == false) || (op == "or" && result == true)
      end
      return nil if result.nil?

      tree["negate"] ? !result : result
    end

    # while/until predicate: a decision plus the loop's zero/entered outcomes.
    def probe_while(key, value)
      target = @probes[key]
      truthy = value ? true : false
      probe_decision(key, value)
      loop_target = target["loop"]
      if loop_target
        enters = loop_target["until"] ? !truthy : truthy
        state_key = [current_context, loop_target["id"]]
        expecting_first = !@loop_state.key?(state_key) || @loop_state[state_key]
        if enters
          hit(current_context, loop_target["entered"]) if expecting_first
          @loop_state[state_key] = false
        else
          hit(current_context, loop_target["zero"]) if expecting_first
          @loop_state[state_key] = true
        end
      end
      value
    end

    # for-loop head: the next body probe decides between zero and entered.
    def probe_for(key, collection)
      target = @probes[key]
      state_key = [current_context, target["id"]]
      hit(current_context, target["zero"]) if @loop_state[state_key] == :pending
      @loop_state[state_key] = :pending
      @settle_lock.synchronize { @pending_loops[state_key] = target["zero"] }
      collection
    end

    def probe_for_body(key)
      target = @probes[key]
      state_key = [current_context, target["id"]]
      hit(current_context, target["entered"]) if @loop_state[state_key] == :pending
      @loop_state[state_key] = :entered
      @settle_lock.synchronize { @pending_loops.delete(state_key) } if @pending_loops.key?(state_key)
      nil
    end

    # A loop reached for the last time in a phase and never entered ran zero
    # times; nothing reaches it again to say so, so the phase's end does.
    def settle_loops(context)
      settled = @settle_lock.synchronize do
        ending = @pending_loops.select { |(ctx, _), _| ctx == context }
        ending.each_key { |state_key| @pending_loops.delete(state_key) }
        ending
      end
      settled.each do |(ctx, id), zero|
        hit(ctx, zero)
        @loop_state[[ctx, id]] = :settled
      end
    end

    # Value-context `&&`/`||`/`||=`/`&&=`: the left operand decides the branch.
    def probe_logical(key, left)
      target = @probes[key]
      truthy = left ? true : false
      short = target["op"] == "or" ? truthy : !truthy
      hit(current_context, short ? target["shortCircuit"] : target["evaluated"])
      left
    end

    # Operator assignment on a target that cannot be re-read: `pre` counts
    # arrivals, `es` counts right sides that started. An arrival whose right
    # side never started was a short-circuit; the check runs at the next
    # arrival and when the phase ends, so recursion inside the right side
    # cannot be mistaken for one.
    def probe_arrival(key)
      target = @probes[key]
      state = arrival_state(current_context, key)
      settle_arrival(current_context, target, state)
      state[0] += 1
      nil
    end

    def probe_evaluation_started(key)
      target = @probes[key]
      state = arrival_state(current_context, key)
      state[1] += 1
      hit(current_context, target["evaluated"])
      nil
    end

    def settle_arrival(context, target, state)
      hit(context, target["shortCircuit"]) if state[0] > state[1]
    end

    def arrival_state(context, key)
      @arrivals[[context, key]] || @settle_lock.synchronize { @arrivals[[context, key]] ||= [0, 0] }
    end

    def settle_arrivals(context)
      arrivals = @settle_lock.synchronize { @arrivals.select { |(ctx, _), _| ctx == context } }
      arrivals.each do |(ctx, key), state|
        settle_arrival(ctx, @probes[key], state)
      end
    end

    def probe_statement(key)
      hit(current_context, @probes[key]["id"])
      nil
    end

    # `&.`: the receiver decides which alternative ran.
    def probe_safe_navigation(key, value)
      target = @probes[key]
      hit(current_context, value.nil? ? target["nil"] : target["called"])
      value
    end

    # Where a branch or method body would start when it has no statement.
    def probe_hits(key)
      @probes[key]["ids"].each { |id| hit(current_context, id) }
      nil
    end

    # rescue flow
    def probe_handler(key, index)
      target = @probes[key]
      hit(current_context, target["raised"])
      handlers = target["handlers"]
      hit(current_context, handlers[index]["selected"])
      handlers[0...index].each { |handler| hit(current_context, handler["missed"]) }
      nil
    end

    def probe_propagated(key)
      target = @probes[key]
      hit(current_context, target["raised"])
      target["handlers"].each { |handler| hit(current_context, handler["missed"]) }
      nil
    end

    def probe_ok(key, value)
      hit(current_context, @probes[key]["success"])
      value
    end

    def probe_ok0(key)
      hit(current_context, @probes[key]["success"])
      nil
    end

    def probe_rescue_modifier(key, value)
      hit(current_context, @probes[key]["raised"])
      value
    end

    # -- transport ----------------------------------------------------------

    def record(entry)
      ensure_transport
      @transport.write(entry)
    end

    def ensure_transport
      pid = Process.pid
      return if @transport && @transport_pid == pid

      # Two threads recording their first records at once each opened an
      # evidence file, and the phases one declared went to the file the
      # other then replaced: the run could not be published.
      @transport_lock.synchronize { open_transport(pid) unless @transport && @transport_pid == pid }
    end

    def open_transport(pid)
      # A forked child inherits the parent's Ruby objects; it must own its
      # own evidence file and re-declare the phase it is running inside.
      forked = !@transport_pid.nil?
      @transport&.close if forked
      worker = forked ? "#{@worker}-#{pid}" : @worker
      @worker = worker
      transport = Transport.new(@evidence_dir, worker, pid)
      # A forked child's evidence file names its ids afresh.
      @id_numbers = {} if forked
      transport.write(
        "t" => "process",
        "v" => EVIDENCE_VERSION,
        "run" => @run_id,
        "pid" => pid,
        "worker" => worker,
        "ruby" => RUBY_VERSION,
        "executable" => RbConfig.ruby,
        "argv" => ARGV.dup,
        "lines" => @probes_supported ? "exact" : "first",
      )
      identity = @identities[@context]
      transport.write({ "t" => "phase", "ctx" => @context, "at" => now_ms }.merge(identity)) if forked && @context != 0 && identity
      # Seen by threads that do not take the lock only once it holds its
      # header.
      @transport = transport
      @transport_pid = pid
    end

    def close
      unmatched = @mutex.synchronize do
        return if @closed

        settle_arrivals(@context)
        settle_loops(@context)
        collect_stdlib(@context, final: true)
        @closed = true
        record("t" => "exit", "at" => now_ms)
        @transport&.close
        @worker == "main" && @saw_file && !@matched_file
      end
      return unless unmatched

      # Every file Ruby loaded lay outside the measured tree, which is what a
      # run reports as zero coverage without a word of explanation. Name the
      # root so the mismatch is visible.
      warn "[supercov] none of the files Ruby loaded lay under the measured root #{@root}"
    end

    def now_ms
      Process.clock_gettime(Process::CLOCK_REALTIME, :millisecond)
    end

    def debug(message)
      warn("[supercov:debug] #{message}") if DEBUG
    end
  end

  # The object bound to $__supercov. Method names are short because they are
  # spliced into user source; each forwards to the runtime.
  class Probe
    def initialize(runtime)
      @runtime = runtime
    end

    def c(key, index, value) = @runtime.probe_condition(key, index, value)
    def d(key, value) = @runtime.probe_decision(key, value)
    def w(key, value) = @runtime.probe_while(key, value)
    def f(key, collection) = @runtime.probe_for(key, collection)
    def fb(key) = @runtime.probe_for_body(key)
    def l(key, left) = @runtime.probe_logical(key, left)
    def pre(key) = @runtime.probe_arrival(key)
    def es(key) = @runtime.probe_evaluation_started(key)
    def s(key) = @runtime.probe_statement(key)
    def n(key, value) = @runtime.probe_safe_navigation(key, value)
    def hs(key) = @runtime.probe_hits(key)
    def h(key, index) = @runtime.probe_handler(key, index)
    def hm(key, value) = @runtime.probe_rescue_modifier(key, value)
    def hm0(key) = @runtime.probe_rescue_modifier(key, nil)
    def p(key) = @runtime.probe_propagated(key)
    def ok(key, value) = @runtime.probe_ok(key, value)
    def ok0(key) = @runtime.probe_ok0(key)
  end

  # The receiver of a project that uses Ractors. A non-main Ractor cannot
  # read $__supercov, but it can call a module, and this one records only in
  # the main Ractor; elsewhere each probe gives back what it was given, as
  # the probe would, and records nothing. A Ractor is shareable, so MAIN can
  # be read from any of them.
  module RactorSafe
    MAIN = Ractor.current

    def self.c(key, index, value) = MAIN.equal?(Ractor.current) ? $__supercov.c(key, index, value) : value
    def self.d(key, value) = MAIN.equal?(Ractor.current) ? $__supercov.d(key, value) : value
    def self.w(key, value) = MAIN.equal?(Ractor.current) ? $__supercov.w(key, value) : value
    def self.f(key, collection) = MAIN.equal?(Ractor.current) ? $__supercov.f(key, collection) : collection
    def self.fb(key) = MAIN.equal?(Ractor.current) ? $__supercov.fb(key) : nil
    def self.l(key, left) = MAIN.equal?(Ractor.current) ? $__supercov.l(key, left) : left
    def self.pre(key) = MAIN.equal?(Ractor.current) ? $__supercov.pre(key) : nil
    def self.es(key) = MAIN.equal?(Ractor.current) ? $__supercov.es(key) : nil
    def self.s(key) = MAIN.equal?(Ractor.current) ? $__supercov.s(key) : nil
    def self.n(key, value) = MAIN.equal?(Ractor.current) ? $__supercov.n(key, value) : value
    def self.hs(key) = MAIN.equal?(Ractor.current) ? $__supercov.hs(key) : nil
    def self.h(key, index) = MAIN.equal?(Ractor.current) ? $__supercov.h(key, index) : nil
    def self.hm(key, value) = MAIN.equal?(Ractor.current) ? $__supercov.hm(key, value) : value
    def self.hm0(key) = MAIN.equal?(Ractor.current) ? $__supercov.hm0(key) : nil
    def self.p(key) = MAIN.equal?(Ractor.current) ? $__supercov.p(key) : nil
    def self.ok(key, value) = MAIN.equal?(Ractor.current) ? $__supercov.ok(key, value) : value
    def self.ok0(key) = MAIN.equal?(Ractor.current) ? $__supercov.ok0(key) : nil

    # What a Ractor runs is not recorded, and the first one each file starts
    # says so: against that file when it is measured source, for the run when
    # a test starts it.
    module Started
      def new(...)
        runtime = MAIN.equal?(Ractor.current) && Supercov.runtime
        runtime&.limitation(
          "ruby-ractor-unrecorded",
          "code run inside a non-main Ractor is not recorded: a Ractor cannot reach Supercov's evidence, so what only a Ractor ran reads as not executed",
          runtime.measured_file(caller_locations(1, 1).first&.absolute_path),
        )
        super
      end
    end
  end

  module Loader
    def load_iseq(path)
      runtime = Supercov.runtime
      if runtime && !runtime.closed
        compiled = runtime.compile(path)
        return compiled if compiled
      end
      defined?(super) ? super : nil
    end

    # Put the hook in front of Ruby's loader and keep it there. A library
    # that prepends its own `load_iseq` later runs before this one, and one
    # that compiles the file itself never reaches it: Bootsnap 1.26.1 does
    # that on every setup, its compile cache on or off, and a Rails app's
    # files then ran without probes. Whatever is prepended after this hook
    # gets a fresh copy of it on top (a module already in the chain is not
    # moved), and the files Supercov does not measure go on to it.
    def self.install
      target = RubyVM::InstructionSequence.singleton_class
      target.prepend(Loader)
      target.define_singleton_method(:prepend) do |*modules|
        result = super(*modules)
        # `super`, Module#prepend itself: the copy goes on top without
        # coming back here.
        super(Loader.dup) if modules.any? { |mod| mod.method_defined?(:load_iseq) || mod.private_method_defined?(:load_iseq) }
        result
      end
    end
  end

  # Runner adapters attach when their classes finish defining.
  module Attach
    def self.install(runtime)
      tracer = TracePoint.new(:end) do |tp|
        name = tp.self.name
        if name == "RSpec::Core::Example"
          require_relative "supercov_rspec"
          Supercov::RSpecAdapter.install(runtime)
        elsif name == "Minitest::Test"
          require_relative "supercov_minitest"
          Supercov::MinitestAdapter.install(runtime)
        elsif name == "Test::Unit::TestCase"
          require_relative "supercov_testunit"
          Supercov::TestUnitAdapter.install(runtime)
        elsif name == "Cucumber::Runtime"
          require_relative "supercov_cucumber"
          Supercov::CucumberAdapter.install(runtime)
        end
      rescue StandardError => error
        runtime.limitation("ruby-runner-adapter-failed", "runner adapter failed to install: #{error.class}: #{error.message}")
      end
      tracer.enable
    end
  end

  # A server's requests, each run as the browser test that sent it: the
  # Playwright runtime names the test in a header on the page's own requests
  # and the request fixture's, and in a cookie on the app's host.
  module Requests
    SCOPE_HEADER = "HTTP_X_SUPERCOV_SCOPE"
    SCOPE_COOKIES = %w[__supercov_scope __supercov_scope_site].freeze
    # The worker a request that names no test is recorded under: numbered
    # within its process, and given by the report to the test that was
    # running when it came, if only one was.
    REQUEST_WORKER = "supercov-request"

    @requests = 0
    @lock = Mutex.new

    # Percent-decoding of a query string's or cookie's value. `uri` is not
    # loaded for it: this runs before Bundler, and loading a default gem here
    # clashes with the version an application's Gemfile pins.
    def self.unescape(value)
      value.tr("+", " ").gsub(/%([0-9A-Fa-f]{2})/) { Regexp.last_match(1).hex.chr }
    end

    # The test a Rack environment names, as a phase identity, or nil.
    def self.identity(env)
      encoded = env[SCOPE_HEADER]
      if encoded.nil? || encoded.empty?
        env["HTTP_COOKIE"].to_s.split(";").each do |part|
          name, value = part.strip.split("=", 2)
          if SCOPE_COOKIES.include?(name) && value && !value.empty?
            encoded = unescape(value)
            break
          end
        end
      end
      return nil if encoded.nil? || encoded.empty?

      values = encoded.split("&").to_h { |pair| pair.split("=", 2).map { |part| unescape(part.to_s) } }
      return nil unless values["v"] == "1" && !values["t"].to_s.empty? && !values["w"].to_s.empty?

      { worker: "playwright:#{values["w"]}", test: values["t"], retry: Integer(values["a"] || "0", exception: false) || 0, phase: "call" }
    rescue ArgumentError
      nil
    end

    # The phase a parent process ran in, from SUPERCOV_CONTEXT: a Ruby
    # parent's identity, or a JavaScript parent's carrier -- base64url JSON
    # holding a Playwright test's scope, or none outside a test.
    def self.inherited(value)
      unless value.start_with?("ey")
        identity = Marshal.load(value.unpack1("m0"))
        raise TypeError, "identity is not a Hash" unless identity.is_a?(Hash)

        return identity
      end
      # Read without `json`, for the reason `unescape` gives: the carrier's
      # ids are plain strings and its retry a number.
      carrier = value.tr("-_", "+/").unpack1("m").force_encoding(Encoding::UTF_8)
      raise TypeError, "not a Supercov carrier" unless carrier.start_with?('{"version":1')

      field = ->(name) { carrier[/"#{name}":"([^"\\]*)"/, 1] }
      return nil unless carrier.include?('"scope":')

      worker, test = field.call("workerId"), field.call("testId")
      raise TypeError, "the carrier's scope names no test" unless worker && test

      { "worker" => "playwright:#{worker}", "test" => test, "retry" => carrier[/"retry":(\d+)/, 1].to_i, "phase" => "call" }
    end

    # Run the block as the test the request names. A request that names no
    # test runs as a numbered request of its own, unless this thread already
    # runs as a test's or a request's: a framework's entry point under a
    # server that was hooked too, or a server a test started.
    def self.serve(runtime, env)
      identity = identity(env)
      current = runtime.own_identity
      if identity.nil?
        return yield unless current.nil?

        number = @lock.synchronize { @requests += 1 }
        identity = { worker: REQUEST_WORKER, test: number.to_s, retry: 0, phase: "call" }
      elsif current && current["worker"] == identity[:worker] && current["test"] == identity[:test] && current["retry"] == identity[:retry]
        return yield
      end
      runtime.switch(identity)
      begin
        yield
      ensure
        if current
          runtime.switch(worker: current["worker"], test: current["test"], retry: current["retry"], phase: current["phase"])
        else
          runtime.switch(nil)
        end
      end
    end

    def self.hook(runtime, owner, method, &environment)
      return if owner.instance_variable_get(:@__supercov_requests)

      owner.instance_variable_set(:@__supercov_requests, true)
      owner.prepend(Module.new do
        define_method(method) do |*args, **kwargs, &block|
          env = environment.call(*args)
          next super(*args, **kwargs, &block) unless env

          Supercov::Requests.serve(runtime, env) { super(*args, **kwargs, &block) }
        end
      end)
    end

    def self.install(runtime)
      tracer = TracePoint.new(:end) do |tp|
        case tp.self.name
        when "Rails::Engine", "Sinatra::Base"
          hook(runtime, tp.self, :call) { |env| env }
        when "Puma::Server"
          hook(runtime, tp.self, :handle_request) { |client, *| client.env if client.respond_to?(:env) }
        when "WEBrick::HTTPServer"
          # WEBrick requests are not Rack environments: read their headers.
          hook(runtime, tp.self, :service) do |request, *|
            { SCOPE_HEADER => request["x-supercov-scope"], "HTTP_COOKIE" => request["cookie"] }
          end
        end
      rescue StandardError => error
        runtime.limitation("ruby-request-adapter-failed", "request adapter failed to install: #{error.class}: #{error.message}")
      end
      tracer.enable
    end
  end

  # Child processes and threads inherit the current phase identity.
  module Propagation
    def self.install(runtime)
      Process.singleton_class.prepend(Module.new do
        define_method(:spawn) do |*args, **kwargs, &block|
          args = Supercov::Propagation.with_environment(runtime, args)
          super(*args, **kwargs, &block)
        end
      end)
      Kernel.singleton_class.prepend(Module.new do
        define_method(:spawn) do |*args, **kwargs, &block|
          args = Supercov::Propagation.with_environment(runtime, args)
          super(*args, **kwargs, &block)
        end
        define_method(:system) do |*args, **kwargs, &block|
          args = Supercov::Propagation.with_environment(runtime, args)
          super(*args, **kwargs, &block)
        end
      end)
      Kernel.prepend(Module.new do
        define_method(:spawn) do |*args, **kwargs, &block|
          args = Supercov::Propagation.with_environment(runtime, args)
          super(*args, **kwargs, &block)
        end
        define_method(:system) do |*args, **kwargs, &block|
          args = Supercov::Propagation.with_environment(runtime, args)
          super(*args, **kwargs, &block)
        end
        # Private, as Kernel's own are: a public `system` would answer it
        # sent to any object.
        private :spawn, :system
      end)
      IO.singleton_class.prepend(Module.new do
        define_method(:popen) do |*args, **kwargs, &block|
          args = Supercov::Propagation.with_environment(runtime, args)
          super(*args, **kwargs, &block)
        end
      end)
      # `exec` replaces this process with another program: no `at_exit`
      # runs, and what this one recorded would go with it. Close the
      # evidence first, as exit would; the new program keeps the identity.
      # (Should `exec` fail and the program go on, the rest of it is not
      # measured.)
      [Kernel, Kernel.singleton_class, Process.singleton_class].each do |owner|
        owner.prepend(Module.new do
          define_method(:exec) do |*args, **kwargs|
            args = Supercov::Propagation.with_environment(runtime, args)
            runtime.close
            super(*args, **kwargs)
          end
          # Kernel#exec is private: a public one would answer `exec` sent to
          # any object, a delegator's database connection's included.
          private :exec if owner == Kernel
        end)
      end
    end

    def self.with_environment(runtime, args)
      additions = runtime.child_environment
      return args if additions.empty?

      if args.first.is_a?(Hash)
        [args.first.merge(additions), *args[1..]]
      else
        [additions, *args]
      end
    end
  end

  @runtime = nil

  # Ruby 3.3 and newer compile instrumented files through the load hook with a
  # probe on every statement; each test is credited with every line it runs.
  def self.probes_supported?
    (RUBY_VERSION.split(".").first(2).map(&:to_i) <=> [3, 3]) >= 0
  end

  def self.runtime
    @runtime
  end

  def self.install
    return @runtime if @runtime

    plan_path = ENV[PLAN_ENV]
    evidence_dir = ENV[EVIDENCE_DIR_ENV]
    run_id = ENV[RUN_ID_ENV]
    return nil if plan_path.to_s.empty? || evidence_dir.to_s.empty? || run_id.to_s.empty?

    worker = ENV[WORKER_ENV].to_s
    if worker.empty?
      number = ENV["TEST_ENV_NUMBER"].to_s
      worker = number.empty? ? "main" : "worker-#{number}"
    end
    keys = !Supercov.probes_supported?
    Coverage.start(oneshot_lines: true, **(keys ? { branches: true, methods: true } : {}))
    $__supercov_lines = {}
    runtime = Runtime.new(plan_path, evidence_dir, run_id, worker)
    $__supercov = Probe.new(runtime)
    Ractor.singleton_class.prepend(RactorSafe::Started) if runtime.plan["ractors"]
    Loader.install
    @runtime = runtime
    inherited = ENV[CONTEXT_ENV]
    if inherited && !inherited.empty?
      begin
        identity = Requests.inherited(inherited)
        if identity
          runtime.switch(worker: identity["worker"], test: identity["test"], retry: identity["retry"], phase: identity["phase"])
          runtime.inherited_identity = runtime.current_identity
        end
      rescue ArgumentError, TypeError, KeyError
        runtime.limitation("ruby-inherited-context-invalid", "SUPERCOV_CONTEXT was not a valid Supercov identity")
      end
    end
    runtime.declare_probe_gap(runtime.plan["probeObligations"] || []) unless runtime.probes_supported?
    runtime.declare_uncountable_lines unless runtime.probes_supported?
    Attach.install(runtime)
    Propagation.install(runtime)
    Requests.install(runtime)
    at_exit { runtime.close }
    runtime
  rescue StandardError => error
    warn("[supercov] Ruby runtime disabled: #{error.class}: #{error.message}")
    nil
  end
end

Supercov.install
