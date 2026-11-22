# frozen_string_literal: true

# Checks a Ruby strong pass by hand, for runners the mutant gem has no
# integration for: every mutant the pass ran is run again on its own, the
# whole suite in a fresh process with only that mutant switched on, and its
# verdict is the suite's exit status. Nothing of the pass's own runner is
# used: no candidate tests, no forks, no stopping at the first failure.
#
#   ruby by-hand.rb <project-dir> <jobs> -- <test command...>
#
# Run it in the project after `supercov runs latest mutants test`: it reads
# that pass's plan and results under .supercov/strong. Prints a JSON
# summary: how many verdicts agree, each that does not, and the mutants the
# suite killed.

require "json"

Encoding.default_external = Encoding::UTF_8

separator = ARGV.index("--") or abort("usage: by-hand.rb <project-dir> <jobs> -- <command...>")
project, jobs = ARGV[0...separator]
command = ARGV[(separator + 1)..]
jobs = Integer(jobs)
pass = Dir[File.join(project, ".supercov/strong/*/ruby/strong-plan.rb")].max_by { |path| File.mtime(path) } or abort("no strong pass")
directory = File.dirname(pass, 2)
plan = Kernel.eval(File.read(pass), TOPLEVEL_BINDING.dup, pass, 1) # rubocop:disable Security/Eval
verdicts = File.readlines(File.join(directory, "results.jsonl")).map { |line| JSON.parse(line) }.reject { |each| each["summary"] }.to_h { |each| [each["id"], each["status"]] }
runtime = File.join(directory, "ruby/runtime/supercov_strong.rb")

run = lambda do |active|
  environment = {
    "RUBYOPT" => "-r#{runtime} #{ENV.fetch('RUBYOPT', '')}".strip,
    "SUPERCOV_STRONG_RUBY_PLAN" => pass,
    "SUPERCOV_STRONG_SWITCHES_ONLY" => "1",
    "SUPERCOV_ACTIVE_MUTANT" => active,
  }
  started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
  pid = Process.spawn(environment, *command, chdir: project, out: File::NULL, err: File::NULL, pgroup: true)
  deadline = started + 120
  status = nil
  loop do
    _, status = Process.wait2(pid, Process::WNOHANG)
    break if status
    if Process.clock_gettime(Process::CLOCK_MONOTONIC) > deadline
      Process.kill("KILL", -pid)
      _, status = Process.wait2(pid)
      return [:timeout, status]
    end
    sleep 0.01
  end
  [status.success? ? :passed : :failed, status]
end

baseline, = run.call("-")
abort("the suite fails without a mutant: nothing to compare") unless baseline == :passed

queue = Queue.new
plan["mutants"].each { |mutant| queue << mutant }
queue.close
checked = Queue.new
Array.new(jobs) do
  Thread.new do
    while (mutant = queue.pop)
      outcome, = run.call("#{mutant['file']}##{mutant['index']}")
      checked << [mutant["id"], outcome]
    end
  end
end.each(&:join)

killed = ->(status) { %w[killed timeout].include?(status) }
disagreements = []
killed_by_hand = []
counts = Hash.new(0)
until checked.empty?
  id, outcome = checked.pop
  by_hand = outcome == :passed ? "survived" : "killed"
  strong = verdicts[id]
  counts["byHand:#{by_hand}"] += 1
  killed_by_hand << id if by_hand == "killed"
  counts["strong:#{strong || 'none'}"] += 1
  next if strong.nil? || strong == "error"
  next if killed.call(strong) == (by_hand == "killed")

  disagreements << { id: id, strong: strong, byHand: by_hand, outcome: outcome }
end
puts JSON.pretty_generate(mutants: plan["mutants"].size, counts: counts, disagreements: disagreements, killed: killed_by_hand.sort)
