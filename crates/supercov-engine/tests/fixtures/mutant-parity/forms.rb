# frozen_string_literal: true

# Less common forms of every kind of node: numbers past 64 bits, parameters
# after a rest, destructuring, safe navigation, clauses without bodies.
class Forms
  def numbers
    [123456789012345678901234567890123456789012, -1.5e-7, 2.5e10, 3r, 1.5r, 2i, 0x7f, 0b101, 0o17]
  end

  def wide_numbers
    [
      170141183460469231731687303715884105727,
      170141183460469231731687303715884105728,
      -170141183460469231731687303715884105729,
      9999999999999999999999999999999999999999,
      10000000000000000000000000000000000000000
    ]
  end

  def no_parentheses a, b
    a + b
  end

  def (Forms).on_parenthesized(a)
    a
  end

  def posts(a, *rest, b, c)
    [a, rest, b, c]
  end

  def destructuring(list)
    list.each { |(a, b), (*c, d), (e, *f, g), (h, *)| [a, b, c, d, e, f, g, h] }
  end

  def keywords(a:, b: 1, **options, &block)
    block.call(a, b, options)
  end

  def no_keywords(a, **nil)
    a
  end

  def forwards(...)
    other(...)
  end

  def anonymous(*, **, &)
    other(*, **, &)
  end

  def blocks(list)
    list.each { |a, | a }
    list.each { |a; b| b = a }
    list.each { || 1 }
    list.each { |(a, b), c| a + b + c }
    list.each { |*a, b| b }
    list.map { _1 + _2 }
    list.map { it + 1 }
    list.each do |a|
      a
    end
    list.each_with_object([]) do |a, memo|
      memo << a
    end
    list.inject(0) { |sum, a| sum + a }
    list.each(1) do
      2
    end
    list.each_slice 2 do |a|
      a
    end
  end

  def lambdas
    -> { 1 }
    ->(a) { a }
    ->(a, b = 1; c) { a + b }
    -> do 2 end
    -> { _1 }
    -> { it }
  end

  def safe(user, value)
    user&.name
    user&.name = value
    user&.name&.upcase
    user&.update(value)
    user&.tags ||= []
    user&.count += 1
  end

  def operator_writes(user, list, value)
    user.name ||= value
    user.name &&= value
    user.count += 1
    list[0] += 1
    list[0] ||= value
    list[0] &&= value
    list[1, 2] -= value
  end

  def index_writes(list, value)
    list[0] = value
    list.[]=(0, value)
    list[] = value
    self[0] = value
  end

  def attribute_writes(user, value)
    user.name = value
    user.name=(value)
    self.name = value
  end

  def multiple(list)
    a, b = list
    *c, d = list
    e, *f, g = list
    h, * = list
    (i, j), k = list
    l, = list
    @m, @@n, $o = list
    list.first, list[1] = list
    [a, b, c, d, e, f, g, h, i, j, k, l]
  end

  def nots(a)
    not(a)
    not()
    !a
  end

  def regexp_matches(text)
    /a/ =~ text
    /(?<word>\w+)/ =~ text
    word
  end

  def supers(a, &block)
    super
    super()
    super(a)
    super(a, &block)
    super(a) { a }
    super { a }
    super(&block)
  end

  def yields(a)
    yield
    yield a
    yield a, key: 1
  end

  def numbered_references
    [$~, $1, $&, $`, $', $+]
  end

  def specials
    [__FILE__, __LINE__, __ENCODING__, __method__]
  end

  def constants
    [Forms, ::Forms, Forms::Inner, ::Forms::Inner, self.class::Inner]
  end

  def definitions(a)
    defined?(a)
    defined?(@b)
    defined?(String)
  end

  def aliases
    alias other_name aliases
    alias $new $old
    undef other_name
    undef :a, :b
  end

  def nested(a)
    def inner(b)
      b
    end
    inner(a)
  end

  def rescue_modifier(a)
    a.call rescue nil
    b = a.call rescue 1
    b
  end

  def ranges(a, b)
    [a..b, a...b, a.., ..b, (a + 1)..(b - 1)]
  end

  def flow(a)
    return if a
    return a, 1 if a
    a.each { |b| next if b }
    a.each { |b| next b if b }
    a.each { |b| break if b }
    a.each { |b| break b if b }
    a.each { |b| redo if b }
    a
  end
end
