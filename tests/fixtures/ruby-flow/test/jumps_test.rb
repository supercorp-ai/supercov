require "minitest/autorun"
require "jumps"

# Converting it raises an error no rescue here selects.
class Boom
  def to_int = raise("boom")
  def to_str = raise("boom")
end

class JumpsTest < Minitest::Test
  def test_each_arm
    %i[unless_arm case_arm pattern_arm nested_arm chain_arm parenthesized].each do |name|
      assert_equal :none, Jumps.public_send(name, nil), name
      assert_equal 7, Jumps.public_send(name, "7"), name
      assert_equal :bad, Jumps.public_send(name, "x"), name
      assert_raises(RuntimeError, name.to_s) { Jumps.public_send(name, Boom.new) }
    end
    assert_equal :empty, Jumps.chain_arm("")
    assert_equal 7, Jumps.modifier("7")
    assert_equal :bad, Jumps.modifier("x")
    assert_equal :none, Jumps.open_arm("0")
    assert_nil Jumps.open_arm("1")
    assert_equal :bad, Jumps.open_arm("x")
    assert_raises(RuntimeError) { Jumps.open_arm(Boom.new) }
  end
end
