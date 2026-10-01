Settings = Struct.new(:mode, :level)

module Writes
  DEFAULT_MODE ||= :auto

  def self.local_or(value)
    found = value
    found ||= :fallback
    found
  end

  def self.local_and(value)
    found = value
    found &&= found.to_s
    found
  end

  def self.global_or
    $writes_seen ||= 0
    $writes_seen += 1
  end

  def self.attribute_or(settings)
    settings.mode ||= :auto
    settings.mode
  end

  def self.attribute_and(settings)
    settings.level &&= settings.level + 1
    settings.level
  end

  def self.index_and(table, key)
    table[key] &&= table[key] * 2
    table[key]
  end

  def self.constant_or
    Writes::LIMIT ||= 10
  end

  def self.first_even(values)
    values.each do |value|
      unless value.odd?
        return value
      end
    end
    nil
  end

  def self.grade(score)
    case score
    when 90.. then return :a
    when 50...90 then :pass
    else
      return :fail
    end
  end

  def self.guarded(value)
    begin
      return :zero if value.zero?
      10 / value
    rescue NoMethodError
      :bad
    else
      :ok
    end
  end

  def self.sum_until(values, stop)
    total = 0
    values.each do |value|
      (break) if value == stop
      total += value
    end
    total
  end
end
