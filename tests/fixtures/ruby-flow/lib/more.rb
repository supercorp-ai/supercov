class Memo
  @@made = 0

  def self.made
    @@made ||= 0
  end

  def value
    @value ||= begin
      @@made += 1
      42
    end
  end

  def reset
    @value &&= nil
  end
end

module More
  def self.unless_not(flag)
    unless !flag
      :on
    else
      :off
    end
  end

  def self.sum_each(values)
    total = 0
    values.each { |value| total += value }
    values.each {}
    total
  end

  def self.kind(value)
    case value
    when Integer then :number
    when String then :text
    else :other
    end
  end

  def self.safe_div(a, b)
    (a / b) rescue nil
  end

  def self.countdown(n)
    seen = []
    while n > 0
      seen << n
      n -= 1
    end
    seen
  end

  def self.early(value)
    label = value.nil? ? (return :missing) : value.to_s
    label.upcase
  end

  def self.pick(value)
    result = case value
             when :skip then return :skipped
             else value
             end
    result
  end

  def self.either(a, b)
    a || b
  end
end
