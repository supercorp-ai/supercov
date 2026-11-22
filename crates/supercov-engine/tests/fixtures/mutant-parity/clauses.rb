# frozen_string_literal: true

# Clauses in every arrangement: rescue, else and ensure with and without
# bodies, case and pattern forms, strings over several lines and escapes.
class Clauses
  def rescues(a)
    begin
    rescue
    end
    begin
    rescue => e
    end
    begin
    rescue ArgumentError, TypeError
    end
    begin
      a.call
    rescue ArgumentError => e
      e
    rescue TypeError => a&.error
      retry
    else
      a
    ensure
      a.close
    end
    begin
      a.call
    ensure
    end
    begin
      a.call
    rescue
    else
    end
    e
  end

  def implicit_rescue(a)
    a.call
  rescue StandardError
    nil
  else
    a
  ensure
    a.close
  end

  def ensure_only(a)
    a.call
  ensure
    a.close
  end

  def block_rescue(list)
    list.each do |a|
      a.call
    rescue StandardError
      nil
    end
  end

  def begins(a)
    begin
    end
    begin
      a
    end
    begin
      a
      a.call
    end
    ()
    (a; a.call)
    a.call while a
    begin
      a.call
    end while a
    begin
      a.call
    end until a
  end

  def loops(list)
    for a in list
      a
    end
    for a, b in list do a end
    while list.any?
      list.pop
    end
    until list.empty?
    end
    loop do
      break
    end
  end

  def cases(a)
    case a
    when 1
    when 2, 3 then a
    end
    case
    when a then 1
    else
    end
    case a
    when 1 then 2
    else 3
    end
  end

  def patterns(value)
    case value
    in Integer | Float => number
      number
    in [1, *rest] if rest.any?
      rest
    in [*, 2] unless value.empty?
      2
    in {name: String => name, **nil}
      name
    in {name:, **rest}
      rest
    in {age: 1, **}
      1
    in ^value
      value
    in [[1, 2], *rest, 3]
      rest
    in nil
    else
      0
    end
  end

  def predicates(value)
    value in [1, *]
    value => {name:}
    name
  end

  def strings(name)
    ["a\tb\r\e\\#{name}", "line one
line two", "ends here
", "two #{name}
lines", "é\x01\x7f", 'it''s', "a" "b", :"a#{name}", %w[a b], %W[a #{name}], %i[a b], %I[a#{name} b]]
  end

  def interpolations(name)
    ["#{}", "#{name; name}", "#{name}", "a#@name", `ls`, `ls #{name}`, ``]
  end

  def regexps(text)
    text.match?(%r{a/b})
    text.match?(%r{a#{text}b})
    text.match?(/a#{text}b/i)
    text.match?(/a/mix)
  end

  def hashes(a, b)
    {a:, b:}
    {"a b": 1, "c": 2}
    {a => 1, :b => 2}
    {nil => 1, self => 2}
    {"a#{b}": 1}
    {}
  end

  def heredocs(name)
    a = <<~TEXT
      plain
    TEXT
    b = <<~TEXT
      one #{name}
      two
    TEXT
    [a, b]
  end

  def conditionals(a, b)
    a ? b : nil
    a ? nil : b
    if a
    elsif b
      1
    elsif a.call
    else
      2
    end
    if a then 1 end
    unless a
      1
    else
      2
    end
    a.call unless b
    a.call if b
    a && b || a.call
    a and b or a.call
  end
end
