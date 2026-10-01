require "minitest/autorun"
require "writes"

class WritesTest < Minitest::Test
  def test_local_writes
    assert_equal :fallback, Writes.local_or(nil)
    assert_equal :given, Writes.local_or(:given)
    assert_nil Writes.local_and(nil)
    assert_equal "1", Writes.local_and(1)
  end

  def test_global_or
    $writes_seen = nil
    assert_equal 1, Writes.global_or
    assert_equal 2, Writes.global_or
  end

  def test_attribute_writes
    assert_equal :auto, Writes.attribute_or(Settings.new(nil, nil))
    assert_equal :fast, Writes.attribute_or(Settings.new(:fast, nil))
    assert_nil Writes.attribute_and(Settings.new(nil, nil))
    assert_equal 2, Writes.attribute_and(Settings.new(nil, 1))
  end

  def test_index_and
    assert_nil Writes.index_and({}, :a)
    assert_equal 4, Writes.index_and({ a: 2 }, :a)
  end

  def test_constant_or
    assert_equal 10, Writes.constant_or
    assert_equal 10, Writes.constant_or
  end

  def test_jumps_in_arms
    assert_equal 2, Writes.first_even([1, 2, 3])
    assert_nil Writes.first_even([1, 3])
    assert_nil Writes.first_even([])
    assert_equal :a, Writes.grade(95)
    assert_equal :pass, Writes.grade(60)
    assert_equal :fail, Writes.grade(10)
    assert_equal :zero, Writes.guarded(0)
    assert_equal :ok, Writes.guarded(5)
    assert_equal :bad, Writes.guarded(nil)
    assert_equal 3, Writes.sum_until([1, 2, 9, 4], 9)
    assert_equal 3, Writes.sum_until([1, 2], 9)
    assert_equal 0, Writes.sum_until([], 9)
    assert_equal :auto, Writes::DEFAULT_MODE
  end
end
