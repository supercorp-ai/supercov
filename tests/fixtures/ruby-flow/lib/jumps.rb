module Jumps
  def self.unless_arm(value)
    begin
      unless value
        return :none
      else
        Integer(value)
      end
    rescue ArgumentError
      :bad
    end
  end

  def self.case_arm(value)
    begin
      case value
      when nil then return :none
      else Integer(value)
      end
    rescue ArgumentError
      :bad
    end
  end

  def self.pattern_arm(value)
    begin
      case value
      in nil then return :none
      else Integer(value)
      end
    rescue ArgumentError
      :bad
    end
  end

  def self.nested_arm(value)
    begin
      begin
        Integer(value)
      rescue TypeError
        return :none
      end
    rescue ArgumentError
      :bad
    end
  end

  def self.chain_arm(value)
    begin
      if value.nil?
        return :none
      elsif value == ""
        return :empty
      else
        Integer(value)
      end
    rescue ArgumentError
      :bad
    end
  end

  def self.parenthesized(value)
    begin
      (value.nil? ? (return :none) : Integer(value))
    rescue ArgumentError
      :bad
    end
  end

  def self.modifier(value)
    return Integer(value) rescue :bad
  end

  def self.open_arm(value)
    begin
      unless Integer(value).positive?
        return :none
      end
    rescue ArgumentError
      :bad
    end
  end
end
