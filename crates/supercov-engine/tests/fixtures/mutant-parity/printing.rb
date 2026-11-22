# frozen_string_literal: true

# Shapes the printer writes back: escapes, floats, conditionals and
# patterns as values, assignment targets and rescue clauses inside nodes a
# mutation replaces.
class Printing
  def escapes(x)
    call("tab\t", "hash \#{x}", "quote \" end", "bell \a", "nul \0", "esc \e")
    call('single \\ quote \'', "unicode é")
  end

  def floats(x)
    call(1e20, 1.0e-5, 3.0, -2.5, 123456789.125)
    x * 1e-10
  end

  def conditional_values(a, b)
    call(if a then 1 else 2 end)
    call(unless a then 3 else 4 end)
    call(a ? b : nil)
    call(case a when 1 then :one else :other end)
    call(while a do break end)
  end

  def keys(a)
    call("a b": 1, "c": a, d: a)
    call({ "x y" => 1, 2 => a })
  end

  def targets(list)
    (a, b), c = list
    d, (e, *f) = list
    g, * = list
    *h, i = list
    j.k, l[0] = list
    [a, b, c, d, e, f, g, h, i]
  end

  def patterns(x)
    case x
    in [a, *]
      a
    in [*, b]
      b
    in { c: nil, **nil }
      nil
    in { d: Integer => d, **rest }
      [d, rest]
    in [Integer, *tail]
      tail
    end
  end

  def clauses(a)
    value = begin
      a.call
    rescue ArgumentError => error
      error.message
    else
      :ok
    ensure
      a.close
    end
    other = begin
      a.call
    ensure
      a.close
    end
    begin
      a.reset
    rescue NameError
    end
    [value, other]
  end

  def numbers(a)
    call(-123_456_789_012_345_678_901_234_567_890)
    call(-1, -a, +a)
    a - -1
  end

  def blocks(list)
    list.each_slice(2).map { |(x, y)| x + y }
    list.inject { |sum, x| sum + x }
    list.each_with_index.select { |x, i| i.even? }.map(&:first)
  end

  def bare_yield_operand(a)
    (yield a) + 1
  end

  def bare_super_operand(a)
    (super a) * 2
  end

  def block_with_ensure(env = {}, &block)
    LOCK.synchronize do
      diff = env.dup
      block.call
    ensure
      restore(diff)
    end
  end

  def bare_yield_receiver
    yield.to_s
  end

  def bare_super_receiver
    super.inspect
  end
end
