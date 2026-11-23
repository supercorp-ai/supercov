# frozen_string_literal: true

# Mutators' less common arms: every operator assignment, numbered
# references, procs, dates, dig, negative and special floats, `it`.
class Edges
  def assignments(a, b)
    a -= 1
    a *= 2
    a /= 3
    a %= 4
    a **= 2
    a &= b
    a |= b
    a ^= b
    a <<= 1
    a >>= 1
    $count += 1
    $count &&= b
    @@total += 1
    @@total ||= 0
    @@total &&= b
    a
  end

  def references(text)
    text =~ /(a)(b)/
    [$1, $2, $3]
  end

  def procs
    proc { 1 }
    Proc.new { 2 }
    ::Proc.new { 3 }
    lambda { 4 }
  end

  def dates(text)
    Date.parse(text)
    ::Date.parse(text)
  end

  def digging(hash)
    hash.dig(:a)
    hash.dig(:a, :b)
    hash.fetch(:a).fetch(:b)
  end

  def floats
    [-1.0, 0.0, -0.0, 1.0, 2.5, Float::INFINITY, -Float::INFINITY, Float::NAN]
  end

  def negations(a)
    !!a
    !a
    not(a)
    -a
    +a
    ~a
  end

  def its(list)
    list.map { it.size }
    list.map { [it, list.map { it * 2 }] }
    list.each_with_object([]) { |x, memo| memo << x }
    list.each { _1 }
  end

  def receivers
    self.foo
    self.class
    self.foo = 1
    self[0]
    nil.to_s
  end

  def mixed(a, b)
    a.b(*b)
    a.b(**b)
    a.b(&b)
    a.b(1, *b)
    a.b(1) { |x| x }
    a.c { |x| next x }
    a.c { break }
    a&.b(1)
    a[1] = 2
    a[1, 2]
    a[]
    a.(1)
    a::b
  end

  def returns(a)
    return
    return a
    return a, 1
  end

  def loops(list)
    for x in list
      next if x
    end
    loop { break }
    list.each do |x|
      redo if x
    end
  end

  def heredocs
    text = <<~ONE
      one #{1}
    ONE
    raw = <<~'TWO'
      two #{2}
    TWO
    [text, raw]
  end

  def strings(a)
    "#{a}"
    "a\nb"
    'single'
    "tab\t"
    :sym
    :"quoted sym"
    %q(percent)
    %Q(percent #{a})
    "#@ivar and #$global"
  end

  def defaults(a = nil, b = [], c = {}, d = :x, *, e: nil, **)
    [a, b, c, d, e]
  end

  def ranges(a)
    (a..)
    (..a)
    (a...)
    1..2
  end

  def begins(a)
    begin
      a
    rescue => error
      error
    end
    begin
      a
    ensure
      a
    end
  end

  def matches(text)
    text.match(/\Afoo/)
    text.match(/foo\z/)
    text =~ /\Abar/
    text.match?(/baz\z/)
    text.match(/\A(?:x)/)
  end

  def defined_checks(a)
    defined?(a)
    defined?(@b)
    defined?(String)
    defined?(a.b)
  end

  def super_calls(a, *b, **c, &d)
    super
    super()
    super(a)
    super(a, *b, **c, &d)
  end

  def sends(a)
    a.to_proc
    a.respond_to?(:x)
    a.is_a?(String)
    a.instance_of?(String)
    a.public_send(:x)
    a.each_with_index.to_a
    a.first(2)
    a.sum
    a.min_by(&:size)
    a.sort_by { |x| x }
  end
end
