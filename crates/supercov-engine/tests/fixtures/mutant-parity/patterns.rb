# frozen_string_literal: true

# Regexp syntax at the edges of what regexp_parser reads: intervals,
# references by number and name, control escapes, sets with classes, nested
# sets and ranges, comments and option groups.
class Patterns
  def intervals(text)
    text =~ /a{2,}b{,3}c{1,2}?d{2}+e{x}f{}g{,}/
  end

  def references(text)
    text =~ /(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)\10\1\12/
    text =~ /(a)\g<1>/
    text =~ /a|b\g<0>/
    text =~ /(?<word>a)\k<word>\g<word>\k'word'/
  end

  def escapes(text)
    text =~ /\cx\x41\x4A\u{41}\p{Alpha}\P{Digit}\0\07/
    text =~ /\h\H\R\X\G\Z\K/
  end

  def sets(text)
    text =~ /[[:alpha:]][a[bc]][\da-f][a\-z][\x41-\x5a][-a]/
    text =~ /[\]\\][a-z&&[^aeiou]]/
  end

  def groups(text)
    text =~ /(?#a comment)(?i)a(?i:b)(?m-x:c)(?=d)(?!e)(?<=f)(?<!g)(?>h)(?~i)/
    text =~ /((a|b)|(c))+/
  end

  def captures(text)
    /(?<=a)(?<!b)(?<word>c)/ =~ text
    word
  end

  def anchors(text)
    text.match?(/^foo/)
    text.match?(/foo$/)
    text.match?(/foo\Z/)
    text.match?(/\Afoo\z/)
    text.match?(/\Afoo/)
    text.match?(/foo\z/)
  end

  def braces(text)
    text =~ /a{b/
    text =~ /{/
    text =~ /x{2}{3}/
  end
end
