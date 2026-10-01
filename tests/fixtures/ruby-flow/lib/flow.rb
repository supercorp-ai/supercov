module Flow
  def self.grade(score)
    unless score.is_a?(Integer)
      return :invalid
    else
      label = case score
              when 90.. then :a
              when 80...90 then :b
              end
    end
    label || :none
  end

  def self.shape(value)
    case value
    in [x] if x.positive?
      :single
    in { name: String => name }
      name.to_sym
    end
  rescue NoMatchingPatternError
    :unknown
  end

  def self.parse(text)
    begin
      Integer(text)
    rescue ArgumentError
      nil
    else
      :parsed
    ensure
      @parsed = true
    end
  end

  def self.first_even(values)
    values.each do |value|
      next if value.odd?
      return value
    end
    nil
  end

  def self.countdown(n)
    steps = []
    until n.zero?
      steps << n
      n -= 1
    end
    steps
  end

  def self.both(a, b)
    !(a && b) ? :not_both : :both
  end
end
