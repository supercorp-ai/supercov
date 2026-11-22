class Methods
  def plain
  end

  def returns_array
    []
  end

  def returns_hash
    {}
  end

  def returns_string
    "a"
  end

  def optional(a, b = 1, *rest, c:, d: 2, **opts, &block)
    [a, b, rest, c, d, opts, block]
  end

  def unused_default(_x = 1)
    _x
  end

  def endless(x) = x + 1

  def forwarding(...)
    other(...)
  end

  def multi(a)
    a.foo
    a.bar
    a.baz
  end

  def lambdas
    -> (x) { x }
    lambda { |y| y }
  end

  def strings(name)
    "#{name}!"
  end

  def numbered(list)
    list.map { _1 * 2 }
  end

  class << self
    def meta(x)
      x
    end
  end
end
