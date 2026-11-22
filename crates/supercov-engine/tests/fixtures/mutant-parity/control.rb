class Control
  def branches(a, b)
    if a
      b
    elsif b
      a
    else
      nil
    end
    return a if b
    a ? b : a
    unless a then b end
  end

  def loops(list)
    while list.any?
      list.pop
    end
    until list.empty?
      break
    end
    begin
      list.shift
    end while list.size > 2
    list.each { |x| next x }
    loop do
      break
    end
    list.each_slice(2).map { _1 }
  end

  def cases(value)
    case value
    when 1, 2 then :low
    when 3 then :mid
    else :high
    end
  end

  def matches(value)
    case value
    in Integer => n if n > 1 then n
    in { name: String } then 1
    end
  end

  def rescues(input)
    Integer(input)
  rescue ArgumentError => error
    error.message
  rescue TypeError
    nil
  else
    1
  ensure
    input.to_s
  end

  def nested(input)
    begin
      input.call
    rescue StandardError
      retry
    end
    input.frozen? rescue false
  end

  def defined_and_super(value)
    defined?(@value)
    super
  end

  def explicit_super(value)
    super(value)
  end

  def yields
    yield
    yield 1, 2
  end
end
