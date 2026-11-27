class Literals
  def integers
    [0, 1, -1, 42, 300, 70_000, 0x10]
  end

  def floats
    [0.0, 1.5, -2.25]
  end

  def strings(name)
    ["", "plain", 'single', "hi #{name}", :sym, :"quoted sym", %w[a b]]
  end

  def booleans
    [true, false, nil]
  end

  def ranges(n)
    [1..n, 1...n, (n..), (..n)]
  end

  def hashes(a, b)
    { a: 1, "b" => b, a => b, **b }
  end

  def regexps(text)
    text =~ /\Afoo/ && text.match?(/bar\z/i)
  end

  def rationals
    [1r, 2i]
  end

  def self.singleton(x)
    x
  end
end

class Patterns
  def words(text)
    text.gsub(/(\w+)\s*-\s*(?<rest>\d+)?/, '\1') =~ /^a|b$|[^\d_a-z]+/
  end

  def anchored(text)
    text.match?(/\Aa*/) || text =~ /(?:x|y)+?\z/ || text.scan(/(?i)[[:alpha:]]{2,3}\./)
  end
end
