require "minitest/autorun"
require "flow"

class FlowTest < Minitest::Test
  def test_grades
    assert_equal :a, Flow.grade(95)
    assert_equal :b, Flow.grade(85)
    assert_equal :none, Flow.grade(10)
    assert_equal :invalid, Flow.grade("x")
  end

  def test_shapes
    assert_equal :single, Flow.shape([2])
    assert_equal :bo, Flow.shape({ name: "bo" })
    assert_equal :unknown, Flow.shape([-1])
    assert_equal :unknown, Flow.shape(3)
  end

  def test_parse
    assert_equal :parsed, Flow.parse("12")
    assert_nil Flow.parse("x")
  end

  def test_loops
    assert_nil Flow.first_even([1, 3])
    assert_equal 4, Flow.first_even([1, 4])
    assert_equal [], Flow.countdown(0)
    assert_equal [2, 1], Flow.countdown(2)
  end

  def test_both
    assert_equal :both, Flow.both(true, true)
    assert_equal :not_both, Flow.both(true, false)
    assert_equal :not_both, Flow.both(false, true)
  end
end
