// Every construct the Ruby instrumenter rewrites, measured end to end.
//
//   node scripts/ruby-syntax-zoo.mjs
//
// A small library whose tests drive each construct both ways: parenthesized
// conditions, `&&`/`||` used as values, a call with a receiver as a
// condition, `not`, pattern matching
// with guards, rescue clauses that retry, `redo` and `break` ending a branch,
// multiple assignment, `alias`, `undef`, endless methods, `Class.new`,
// refinements, safe navigation with a block, method and block bodies that
// rescue, and `begin ... end while`. Each file is read back for what its tests
// showed: a construct measured wrongly reads as uncovered here.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, resolve } from 'node:path';
import { assessWithStandIn } from './assess-with-stand-in.mjs';

const repository = resolve(import.meta.dirname, '..');
const binary = process.env.SUPERCOV_BINARY ?? resolve(repository, `target/debug/supercov${process.platform === 'win32' ? '.exe' : ''}`);
const temporary = mkdtempSync(resolve(realpathSync.native(tmpdir()), 'supercov-ruby-syntax-zoo-'));
const root = resolve(temporary, 'zoo');

// The Ruby the other Ruby gates measure: the one named, Homebrew's, or PATH's.
function ruby() {
  const candidates = process.env.SUPERCOV_RUBY ? [process.env.SUPERCOV_RUBY] : ['/opt/homebrew/opt/ruby/bin/ruby', 'ruby'];
  for (const candidate of candidates) {
    const asked = spawnSync(candidate, ['-e', 'print RUBY_VERSION'], { encoding: 'utf8' });
    const [major, minor] = (asked.stdout ?? '').split('.').map(Number);
    if (major > 3 || (major === 3 && minor >= 3)) return candidate;
  }
  throw new Error('ruby-syntax-zoo needs Ruby 3.3 or newer (or SUPERCOV_RUBY)');
}

const files = {
  'lib/flow.rb': `module Flow
  def self.grouped(a, b)
    if (a)
      'a'
    elsif (b; a.nil?)
      'b'
    else
      'none'
    end
  end

  def self.ready(item)
    if item.ready?
      'go'
    else
      'wait'
    end
  end

  def self.negated(a, b)
    return 'neither' if not (a or b)
    return 'first' if not not a
    'second'
  end

  def self.classify(value)
    case value
    in Integer => n unless n.zero? then 'number'
    in Integer then 'zero'
    in String if value.empty? then 'blank'
    else 'other'
    end
  end

  def self.first_set(a, b, default)
    a || b || default
  end

  def self.grouped_value(a, b, c)
    (a && b) || c
  end

  def self.neither(a, b)
    !(a && b)
  end

  def self.choose(flag)
    unless flag then 'off' else 'on' end
  end

  def self.parse(text)
    attempts = 0
    begin
      attempts += 1
      raise KeyError, 'missing' if text == :missing
      raise ArgumentError, 'flaky' if text == 'flaky' && attempts < 2
      value = Integer(text)
    rescue ArgumentError, TypeError => error then
      retry if error.message == 'flaky'
      nil
    else
      value * 2
    ensure
      $attempts = attempts
    end
  end

  def self.redo_once(items)
    seen = []
    again = false
    items.each do |item|
      seen << item
      if item == :again && !again
        again = true
        redo
      end
    end
    seen
  end

  def self.first_big(values)
    found = nil
    values.each do |value|
      if value > 10
        found = value
        break
      end
    end
    found
  end

  def self.safe_divide(a, b)
    a / b
  rescue ZeroDivisionError
    :infinite
  end

  def self.integers(list)
    list.map do |item|
      Integer(item)
    rescue ArgumentError
      0
    end
  end

  def self.countdown(n)
    steps = 0
    begin
      steps += 1
      n -= 1
    end while n > 0
    steps
  end

  def self.upcased(list) = list&.map { |item| item.upcase }
end
`,
  'lib/shapes.rb': `class Pair
  def initialize(pair)
    @left, @right = pair
  end

  def left = @left

  def sum = @left + @right

  alias total sum

  def scratch; end

  undef scratch
end

Rectangle = Class.new do
  def area(width, height) = width > 0 && height > 0 ? width * height : 0
end

module Shout
  refine String do
    def shout = upcase + (end_with?('!') ? '' : '!')
  end
end

using Shout

module Loud
  def self.say(text) = text.shout
end

$zoo_greeting = 'hello'
alias $zoo_hello $zoo_greeting
`,
  'lib/literals.rb': `module Literals
  def self.constant(kind)
    case kind
    when :true then (true ? :yes : :no)
    when :integer then (1 ? :yes : :no)
    when :float then (1.0 ? :yes : :no)
    when :rational then (1r ? :yes : :no)
    when :imaginary then (1i ? :yes : :no)
    when :string then ('s' ? :yes : :no)
    when :symbol then (:s ? :yes : :no)
    when :false then (false ? :yes : :no)
    else (nil ? :yes : :no)
    end
  end
end
`,
  // Shapes beside the ones above: empty branches, a rescue naming its error
  // with \`then\`, jumps that end an arm, and calls that look like the
  // special ones (\`Class.new\`, \`refine\`, \`!\`) but are not.
  'lib/edges.rb': `module Edges
  class Base; end

  def self.empty_branches(flag)
    if flag
    else
    end
    unless flag
    end
    flag ? :yes : :no
  end

  def self.rescued(kind)
    raise ArgumentError, 'bad' if kind == :argument
    raise KeyError, 'missing' if kind == :key
    raise TypeError, 'other' if kind == :type
    :none
  rescue ArgumentError => error then error.message
  rescue KeyError
    :key
  end

  def self.pair_break(items)
    items.each { |item| break item, item * 2 if item > 1 }
  end

  def self.case_jump(items)
    found = []
    items.each do |item|
      case item
      when 0 then next
      when 9 then break
      else found << item
      end
    end
    found
  end

  def self.loop_jump(items)
    items.map do |item|
      if item.nil?
        next 0
      else
        item
      end
    end
  end

  def self.plain_new = Class.new(Base)

  def self.not_with(value) = value.!

  def self.endless(value) = (value + 1)

  # Empty handlers: the probe goes after \`then\`, the name, the class or the
  # keyword, whichever the clause ends with.
  def self.quiet_then(fail)
    raise fail if fail
  rescue RuntimeError => error then
  end

  def self.quiet_named(fail)
    raise fail if fail
  rescue RuntimeError => error
  end

  def self.quiet_class(fail)
    raise fail if fail
  rescue RuntimeError
  end

  def self.quiet_bare(fail)
    raise fail if fail
  rescue
  end

  def self.literal_unless(value)
    unless true
      value = :never
    else
      value = value.to_s
    end
    unless false
      value = value.upcase
    end
    value
  end
end
`,
  'test/zoo_test.rb': `require 'minitest/autorun'
require 'flow'
require 'shapes'
require 'literals'
require 'edges'

Ready = Struct.new(:ready?)

class ZooTest < Minitest::Test
  def test_conditions
    assert_equal 'a', Flow.grouped(true, false)
    assert_equal 'b', Flow.grouped(nil, true)
    assert_equal 'none', Flow.grouped(false, true)
    assert_equal 'b', Flow.grouped(nil, false)
    assert_equal 'go', Flow.ready(Ready.new(true))
    assert_equal 'wait', Flow.ready(Ready.new(false))
    assert_equal 'neither', Flow.negated(false, false)
    assert_equal 'first', Flow.negated(true, false)
    assert_equal 'second', Flow.negated(false, true)
    assert_equal 'off', Flow.choose(false)
    assert_equal 'on', Flow.choose(true)
    # Used as values, they return their operands, not their truth.
    assert_equal 'x', Flow.first_set('x', nil, 'd')
    assert_equal 'y', Flow.first_set(nil, 'y', 'd')
    assert_equal 'd', Flow.first_set(nil, false, 'd')
    assert_equal [2], Flow.grouped_value([1], [2], 'c')
    assert_equal 'c', Flow.grouped_value(nil, [2], 'c')
    assert_equal 'c', Flow.grouped_value([1], false, 'c')
    assert_equal false, Flow.neither(1, 1)
    assert_equal true, Flow.neither(nil, 1)
    assert_equal true, Flow.neither(1, nil)
  end

  def test_patterns
    assert_equal 'number', Flow.classify(3)
    assert_equal 'zero', Flow.classify(0)
    assert_equal 'blank', Flow.classify('')
    assert_equal 'other', Flow.classify('x')
  end

  def test_jumps_and_rescue
    assert_equal 8, Flow.parse('4')
    assert_nil Flow.parse('flaky')
    assert_equal 2, $attempts
    assert_nil Flow.parse(nil)
    assert_raises(KeyError) { Flow.parse(:missing) }
    assert_equal [], Flow.redo_once([])
    assert_nil Flow.first_big([])
    assert_equal [], Flow.integers([])
    assert_raises(TypeError) { Flow.integers([nil]) }
    assert_raises(TypeError) { Flow.safe_divide(1, nil) }
    assert_equal %i[a again again], Flow.redo_once(%i[a again])
    assert_equal %i[a], Flow.redo_once(%i[a])
    assert_equal 11, Flow.first_big([1, 11, 12])
    assert_nil Flow.first_big([1])
    assert_equal 2, Flow.safe_divide(4, 2)
    assert_equal :infinite, Flow.safe_divide(1, 0)
    assert_equal [1, 0], Flow.integers(%w[1 x])
    assert_equal 3, Flow.countdown(3)
    assert_equal 1, Flow.countdown(0)
    assert_equal %w[A], Flow.upcased(%w[a])
    assert_nil Flow.upcased(nil)
  end

  def test_shapes
    pair = Pair.new([1, 2])
    assert_equal 1, pair.left
    assert_equal 3, pair.total
    refute_respond_to pair, :scratch
    assert_equal 6, Rectangle.new.area(2, 3)
    assert_equal 0, Rectangle.new.area(0, 3)
    assert_equal 0, Rectangle.new.area(2, 0)
    assert_equal 'HI!', Loud.say('hi')
    assert_equal 'HI!', Loud.say('hi!')
    assert_equal 'hello', $zoo_hello
  end

  def test_edges
    assert_equal :yes, Edges.empty_branches(true)
    assert_equal :no, Edges.empty_branches(false)
    assert_equal 'bad', Edges.rescued(:argument)
    assert_equal :key, Edges.rescued(:key)
    assert_equal :none, Edges.rescued(:other)
    assert_raises(TypeError) { Edges.rescued(:type) }
    assert_equal [], Edges.pair_break([])
    assert_equal [], Edges.case_jump([])
    assert_equal [], Edges.loop_jump([])
    assert_equal [2, 4], Edges.pair_break([1, 2])
    assert_equal [1], Edges.pair_break([1])
    assert_equal [1, 2], Edges.case_jump([0, 1, 2])
    assert_equal [1], Edges.case_jump([1, 9, 2])
    assert_equal [0, 3], Edges.loop_jump([nil, 3])
    assert_equal Edges::Base, Edges.plain_new.superclass
    assert_equal true, Edges.not_with(nil)
    assert_equal false, Edges.not_with(1)
    assert_equal 3, Edges.endless(2)
    %i[quiet_then quiet_named quiet_class quiet_bare].each do |name|
      assert_nil Edges.public_send(name, RuntimeError)
      assert_nil Edges.public_send(name, nil)
      assert_raises(NotImplementedError) { Edges.public_send(name, NotImplementedError) }
    end
    assert_equal 'AB', Edges.literal_unless(:ab)
  end

  def test_literals
    assert_equal %i[yes yes yes yes yes yes yes no no],
                 %i[true integer float rational imaginary string symbol false nil].map { |kind| Literals.constant(kind) }
  end
end
`,
};

const run = (command, args) => spawnSync(command, args, { cwd: root, encoding: 'utf8', env: { ...process.env, NO_COLOR: '1', CI: '1' } });

try {
  for (const [file, text] of Object.entries(files)) {
    mkdirSync(dirname(resolve(root, file)), { recursive: true });
    writeFileSync(resolve(root, file), text);
  }
  run('git', ['init', '-q']);
  const interpreter = ruby();
  const command = [interpreter, '-Ilib', '-Itest', 'test/zoo_test.rb'];
  const plain = run(command[0], command.slice(1));
  assert.equal(plain.status, 0, `${plain.stdout}${plain.stderr}`);
  const measured = run(binary, ['--', ...command]);
  assert.equal(measured.status, 0, `${measured.stdout}${measured.stderr}`);
  const listed = JSON.parse(run(binary, ['runs', 'latest', 'files', '--json', '--limit', '100']).stdout).data.files;
  const byFile = Object.fromEntries(listed.map((entry) => [entry.file, entry]));
  const report = listed.map((entry) => `${entry.file}: lines ${entry.covered.lines}/${entry.totals.lines} branches ${entry.covered.branches}/${entry.totals.branches} MC/DC ${entry.covered.mcdcConditions}/${entry.totals.mcdcConditions} limits ${entry.measurementLimitations}`).join('\n');
  if (process.env.ZOO_REPORT) console.log(report);
  // Each construct was driven both ways, so nothing is left uncovered.
  for (const file of ['lib/flow.rb', 'lib/shapes.rb', 'lib/literals.rb', 'lib/edges.rb']) {
    const entry = byFile[file];
    assert.ok(entry, `${file} was not measured\n${report}`);
    for (const metric of ['lines', 'branches', 'mcdcConditions']) {
      assert.equal(entry.covered[metric], entry.totals[metric], `${file}: ${metric}\n${report}`);
    }
    assert.ok(entry.totals.lines > 0, `${file} has no lines\n${report}`);
    // Probed source Ruby refuses falls back to line coverage alone, and says so.
    assert.equal(entry.measurementLimitations, 0, `${file}: limitations\n${report}`);
  }
  // Every construct is read for what a test would have to catch, too.
  const assessed = await assessWithStandIn(binary, root, {});
  assert.equal(assessed.status, 0, assessed.output);
  assert.match(assessed.output, /\b100% asserted \((\d+) of \1 executed statements\)/, assessed.output);
  console.log('[ruby-syntax-zoo] every construct the Ruby instrumenter rewrites reads as its tests drove it, and is assessed');
} finally {
  if (!process.env.KEEP) rmSync(temporary, { recursive: true, force: true });
}
