class Sends
  def calls(list, other)
    list.map(&:to_s).select { |item| item.empty? }.first
    other.to_a
    other.to_h
    other.to_i
    other.dig(:a, :b)
    send(:foo, 1)
    Array(list)
    list.reduce(:+)
    list.reduce(0, :+)
    Kernel.const_get(:String)
    self.helper(list)
    proc { list }
    list.fetch(1, other)
  end

  def operators(a, b)
    total = a + b - a * b / 2 % 3 ** 2
    total << 1
    a == b
    a != b
    a < b || a <= b && a > b and a >= b
    a === b
    !a
    !!b
    -a
    a & b | b ^ a
    a.eql?(b)
  end

  def attributes(record, value)
    record.name = value
    record[1] = value
    record[1]
    record[1..-1]
    record&.name
    @ivar = record
    @@cvar = record
    $gvar = record
    x = record
    x
  end

  def assignments(a, b)
    a += 1
    a ||= b
    a &&= b
    @c.d += 2
    a, b = b, a
    CONSTANT
  end

  def helper(list)
    list.each_with_index do |(left, right), index|
      yield left, right, index
    end
  end
end
