# frozen_string_literal: true

# Regexp bodies mutant's regexp mutators walk: anchors, classes, sets,
# groups, quantifiers, escapes and references.
class Regexps
  def anchors(text)
    text =~ /^a$/
    text =~ /\Aa\z/
    text =~ /\Aa\Z/
    text =~ /\ba\B/
    text =~ /\Gab/
  end

  def classes(text)
    text =~ /\d\D\w\W\s\S\h\H/
    text =~ /\R\X/
    text =~ /./m
    text =~ /\p{Alpha}\P{Digit}\p{^Space}/
  end

  def sets(text)
    text =~ /[abc]/
    text =~ /[^abc]/
    text =~ /[a-z0-9_]/
    text =~ /[\d\s]/
    text =~ /[[:alpha:]][[:^digit:]]/
    text =~ /[a-z&&[^aeiou]]/
    text =~ /[\]\\\-]/
  end

  def groups(text)
    text =~ /(a)(?:b)(?<name>c)/
    text =~ /(?=a)(?!b)(?<=c)(?<!d)/
    text =~ /(?>a+)/
    text =~ /(?i)a(?-i:b)/
    text =~ /(a|b|c)/
    text =~ /a|b/
    text =~ /(?#comment)a/
  end

  def quantifiers(text)
    text =~ /a*b+c?/
    text =~ /a*?b+?c??/
    text =~ /a*+b++c?+/
    text =~ /a{2}b{2,}c{,3}d{2,3}/
    text =~ /a{2}?b{2,3}+/
    text =~ /(ab)*/
    text =~ /[ab]+/
  end

  def escapes(text)
    text =~ /\t\n\r\f\v\e\a/
    text =~ /\x41B\u{43}/
    text =~ /\101/
    text =~ /\.\*\+\?\(\)\[\]\{\}\/\|\^\$/
    text =~ /\0/
  end

  def references(text)
    text =~ /(a)\1/
    text =~ /(?<x>a)\k<x>/
    text =~ /(?<x>a)\k'x'/
    text =~ /(?<x>a)\g<x>/
    text =~ /(a)\g'1'/
  end

  def literals(text)
    text =~ /abc+/
    text =~ /x{y}/
    text =~ /(a)(b)(c)(d)(e)(f)(g)(h)(i)(j)\10/
    text =~ /abc/
    text =~ / /
    text =~ /a b/x
  end
end
