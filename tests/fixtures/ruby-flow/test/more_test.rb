require "minitest/autorun"
require "more"

class MoreTest < Minitest::Test
  def test_memo
    memo = Memo.new
    assert_equal 42, memo.value
    assert_equal 42, memo.value
    assert_nil memo.reset
    assert_nil Memo.new.reset
    assert_kind_of Integer, Memo.made
  end

  def test_unless_not
    assert_equal :on, More.unless_not(true)
    assert_equal :off, More.unless_not(false)
  end

  def test_sum_each
    assert_equal 6, More.sum_each([1, 2, 3])
  end

  def test_sum_each_empty
    assert_equal 0, More.sum_each([])
  end

  def test_kind
    assert_equal :number, More.kind(1)
    assert_equal :text, More.kind("a")
    assert_equal :other, More.kind(nil)
  end

  def test_safe_div
    assert_equal 2, More.safe_div(4, 2)
    assert_nil More.safe_div(1, 0)
  end

  def test_countdown
    assert_equal [2, 1], More.countdown(2)
    assert_equal [], More.countdown(0)
  end

  def test_early_and_pick
    assert_equal :missing, More.early(nil)
    assert_equal "AB", More.early(:ab)
    assert_equal :skipped, More.pick(:skip)
    assert_equal :kept, More.pick(:kept)
  end

  def test_either
    assert_equal 1, More.either(1, 2)
    assert_equal 2, More.either(nil, 2)
    assert_nil More.either(nil, nil)
  end
end
