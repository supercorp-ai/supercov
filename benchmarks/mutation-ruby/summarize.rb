# frozen_string_literal: true

# Summarizes one project's join of the mutant gem's verdicts with
# Supercov's strong pass: counts, every disagreement, and the times.
#
#   ruby summarize.rb join.json join.jsonl project mutant-seconds measured-seconds strong-seconds jobs \
#     plain-seconds unprobed-seconds probed-seconds unprobed-log probed-log

require "json"

join_path, lines_path, project, mutant_seconds, measured_seconds, strong_seconds, jobs,
  plain_seconds, unprobed_seconds, probed_seconds, unprobed_log, probed_log = ARGV
# The test command's share of a Supercov run, from its timings line.
tests_ms = ->(log) { File.read(log)[/timings .*tests=([\d.]+)ms/, 1]&.to_f rescue nil }
data = JSON.parse(File.read(join_path))["data"]
data["mutants"] = File.readlines(lines_path).map { |line| JSON.parse(line) }
PROBED = %w[replacement-same replacement-differs undecided not-evaluated].freeze
killed = ->(status) { %w[killed timeout].include?(status) }
mutant_killed = ->(status) { %w[Killed Timeout].include?(status) }
disagreements = []
counts = Hash.new(0)
data["mutants"].each do |mutant|
  strong = mutant.dig("strong", "status")
  counts["supercov:#{strong || 'none'}"] += 1
  reference = mutant["stryker"]
  next unless reference

  counts["matched"] += 1
  counts["mutant:#{reference}"] += 1
  next if strong.nil? || strong == "error"
  next if killed.call(strong) == mutant_killed.call(reference)

  disagreements << { id: mutant["id"], supercov: strong, mutant: reference, reason: mutant.dig("strong", "reason"), killedBy: mutant.dig("strong", "killedBy") }
end
errors = data["mutants"].select { |mutant| mutant.dig("strong", "status") == "error" && mutant["stryker"] }
                        .map { |mutant| { id: mutant["id"], mutant: mutant["stryker"], reason: mutant.dig("strong", "reason") } }
puts JSON.pretty_generate(
  project: project,
  mutants: data["mutants"].size,
  counts: counts,
  join: data["stryker"]&.slice("matched", "onlySupercov", "onlyStryker"),
  # A mutant mutant kills that the weak verdict calls not infected or not
  # reached, or the reverse for an equivalent one.
  weakSoundnessViolations: data.dig("stryker", "soundnessViolations"),
  weak: data["mutants"].group_by { |mutant| mutant["status"] }.transform_values(&:size),
  # Mutants a value probe decided, by the weak verdict it gave.
  probed: data["mutants"].select { |mutant| PROBED.include?(mutant.dig("weak", "code")) }
                         .group_by { |mutant| mutant["status"] }.transform_values(&:size),
  killedBySupercovSurvivedMutant: disagreements.count { |each| killed.call(each[:supercov]) },
  survivedSupercovKilledByMutant: disagreements.count { |each| !killed.call(each[:supercov]) },
  disagreements: disagreements,
  errorsMutantJudged: errors,
  seconds: { mutant: mutant_seconds.to_f, supercovMeasured: measured_seconds.to_f, supercovStrong: strong_seconds.to_f },
  # Medians of five runs: the plain suite, Supercov without and with the
  # value probes.
  overhead: { plain: plain_seconds.to_f, unprobed: unprobed_seconds.to_f, probed: probed_seconds.to_f,
              testsMs: { unprobed: tests_ms.call(unprobed_log), probed: tests_ms.call(probed_log) } },
  jobs: jobs.to_i
)
