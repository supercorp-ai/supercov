# frozen_string_literal: true

# Every method name mutant swaps for another, called once each.
class Selectors
  def collections(list, hash)
    list.all?
    list.none?
    list.append(1)
    list.prepend(1)
    list.assoc(1)
    list.rassoc(1)
    list.chunk(&:odd?)
    list.chunk_while(&:odd?)
    list.collect(&:odd?)
    list.collect!(&:odd?)
    list.collect_concat(&:odd?)
    list.compact!
    list.count
    list.delete_if(&:odd?)
    list.detect(&:odd?)
    list.drop(1)
    list.each_cons(2)
    list.filter(&:odd?)
    list.filter!(&:odd?)
    list.filter_map(&:odd?)
    list.find(&:odd?)
    list.flat_map(&:odd?)
    list.flatten!
    list.grep(1)
    list.grep_v(1)
    list.keep_if(&:odd?)
    list.map!(&:odd?)
    list.max
    list.max_by(&:odd?)
    list.min
    list.reject(&:odd?)
    list.reject!(&:odd?)
    list.reverse!
    list.reverse_each(&:odd?)
    list.reverse_map(&:odd?)
    list.rotate!
    list.sample
    list.select!(&:odd?)
    list.shuffle!
    list.slice_after(1)
    list.slice_before(1)
    list.slice_when(&:odd?)
    list.sort!
    list.sort_by!(&:odd?)
    list.take(1)
    list.uniq!
    list.unshift(1)
    list.values_at(1)
    hash.each_key(&:odd?)
    hash.each_value(&:odd?)
    hash.keys
    hash.values
    hash.reverse_merge(hash)
    hash.transform_keys(&:odd?)
    hash.transform_keys!(&:odd?)
    hash.transform_values(&:odd?)
    hash.transform_values!(&:odd?)
  end

  def strings(text)
    text.bytes
    text.chars
    text.capitalize!
    text.chomp!
    text.chop!
    text.delete!('a')
    text.downcase
    text.downcase!
    text.encode!('a')
    text.gsub!('a', 'b')
    text.lstrip!
    text.rstrip!
    text.scrub!
    text.squeeze!
    text.strip!
    text.sub!('a', 'b')
    text.swapcase!
    text.tr!('a', 'b')
    text.tr_s!('a', 'b')
    text.unicode_normalize!
    text.upcase
  end

  def numbers(number, other)
    number >> other
    number.ceil
    number.floor
    number.negative?
    number.positive?
    number.odd?
    number.pred
    number.succ
    number.zero?
    number.kind_of?(other)
  end
end
