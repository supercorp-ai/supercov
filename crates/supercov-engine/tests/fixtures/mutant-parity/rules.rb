# frozen_string_literal: true

# The conditions inside mutant's mutators: calls by name, reductions,
# identities, assignments on the left of `and`, blocks of one call, ternaries
# under `return`, optional parameters, single-pattern `case ... in`.
class Rules
  def by_name(target, name)
    target.send(name)
    target.public_send(:size, 1)
    target.const_get(name)
    target.const_get(:Inner)
    !target.ready?
    Thing.new { 1 }
    target.new
    target.reduce(&:+)
    target.reduce(0, &:+)
    target.reduce(:+)
    target.reduce(:*)
    target.defined?
    target.other("a" => 1, name => 2)
  end

  def identities(a)
    [a * 1, a / 1, a * -1, a / -1, a * 1.0, a / -1.0, a * 2]
  end

  def left_assignments(list)
    a = list.first and a.call
    b = list.last or b.call
    (list.first..list.last) && list.call
    (list.first..) && list.call
  end

  def single_calls(list)
    list.each { list.clear }
    list.each { next }
    list.each { break if list.empty? }
    -> { list.clear }
  end

  def ternaries(a)
    return a ? 1 : 2 if a.nil?
    return (1 if a) if a.zero?
    a.each { |b| next b ? 1 : 2 }
    a.each { |b| break (1 unless b) }
  end

  def defaulted(a = 1, _b = 2)
  end

  def integer_body
    1
  end

  def float_body
    1.5
  end

  def single_pattern(a)
    case a
    in 1 then 2
    end
  end

  def indices(list)
    [list[1..-1], list[1..], list[a = 1], [list], [*list]]
  end

  def self.singleton_super(a)
    super
  end

  def imaginary
    [0i, 1i, -2i, -1.5r, -3]
  end
end
