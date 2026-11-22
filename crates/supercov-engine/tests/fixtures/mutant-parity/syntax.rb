# frozen_string_literal: true

# Syntax each converter, mutator and printer arm meets: assignment targets,
# parameters, string shapes, literals and references.
class Syntax
  def multiple(list, pair)
    a, (b, c), *d = list
    @x, $y, @@z = pair
    self.name, list[0] = pair
    first, * = list
    *, last = list
    e, *f, g = list
    [a, b, c, d, e, f, g, first, last]
  end

  def parameters(a, b = 1, *rest, c, d:, e: 2, **opts, &block)
    [a, b, rest, c, d, e, opts, block]
  end

  def no_keywords(a, **nil)
    a
  end

  def anonymous(*, **, &)
    inner(*, **, &)
  end

  def forward(...)
    inner(...)
  end

  def blocks(list)
    list.each { |(x, y), z| x + y + z }
    list.map { |x; shadow| shadow = x }
    list.map { it + 1 }
    list.map { _1 * 2 }
    list.each_with_object({}) { |(key, *values), memo| memo[key] = values }
    list.each { |*all| all }
    list.each { |a, (b, *c), d| [a, b, c, d] }
    lambda { |x| x }.call(1)
    ->(y) { y * 2 }
  end

  def strings(name)
    plain = "plain"
    interpolated = "a #{name} b"
    adjacent = "one" "two"
    heredoc = <<~TEXT
      first #{name}
      second
    TEXT
    squiggly = <<-RAW
      raw line
    RAW
    command = `echo hi`
    dynamic_command = `echo #{name}`
    symbol = :"sym_#{name}"
    chars = %w[a b c]
    symbols = %i[x y]
    [plain, interpolated, adjacent, heredoc, squiggly, command, dynamic_command, symbol, chars, symbols]
  end

  def references
    $~
    $1
    $&
    @@counter
    $global
    __FILE__
    __LINE__
    __method__
    defined?(@ivar)
  end

  def numbers
    [123_456_789_012_345_678_901_234_567_890, -98_765_432_109_876_543_210_987, 1r, 2.5r, 3i, 1.5e10, 0x1f, 0b101, 0o17]
  end

  def regexps(text)
    if /(?<word>\w+)/ =~ text
      word
    end
    %r{a/b}i.match(text)
    /#{text}x/
    /a+b*/o
  end

  def ranges(n)
    [1..n, 1...n, (n..), (..n), "a".."z"]
  end

  def conditionals(a, b)
    c = a ? b : nil
    d = (a or b)
    e = (not a)
    f = !!b
    g = a && !b
    unless a
      b
    else
      c
    end
    while a
      break if b
      next
    end
    until b
      redo if a
    end
    begin
      a
    end while b
    [c, d, e, f, g]
  end

  def cases(value)
    case value
    when 1, 2 then :small
    when Integer then :number
    else :other
    end
    case value
    in [x, *rest] if x > 0
      rest
    in { name: String => name }
      name
    in Integer | Float => number
      number
    in ^value
      value
    else
      nil
    end
  end

  def rescues(a)
    a.call
  rescue ArgumentError, TypeError => error
    retry if error.message.empty?
    error
  rescue
    nil
  else
    :ok
  ensure
    a.close
  end

  def operators(a, b)
    a += 1
    b ||= 2
    a &&= b
    @count ||= 0
    @count += 1
    $total ||= []
    a.size += 1
    a[0] ||= b
    a&.size
    a&.size += 1
    a.b, c = 1, 2
    [a, b, c]
  end

  def calls(a)
    a.public_send(:name, 1)
    a.send(:name)
    a.__send__(:name)
    a.method(:name)
    a.to_s
    a.to_i
    a.to_a
    a.to_h
    a.to_f
    a.reduce(:+)
    a.reduce(0, :+)
    a.inject(&:+)
    Array(a)
    Kernel.Array(a)
    a.match(/\Afoo/)
    a.match?(/bar\z/)
    a =~ /\Abaz/
    a.fetch(1)
    a.at(1)
    a.first.last
    super
  end

  def yields(a)
    yield a
    yield
    block_given?
    super(a)
    super()
  end

  def self.on_class(a)
    a
  end

  class << self
    def in_singleton(a)
      a * 2
    end
  end

  def constants
    ::TopLevel
    Outer::Inner
    self.class::Nested
  end

  def hashes(a, b)
    { a: 1, "b" => 2, a => b, **b }
    { a:, b: }
    f(a: 1, **b)
    f({ a: 1 })
    [*a, *b]
    [1, *a]
  end

  def endless(a) = a + 1

  CONST_B = 1
  Outer::CONST_C ||= 2

  def flip_flop(a)
    a.select { |x| (x == 1)..(x == 3) }
  end
end
