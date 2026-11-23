# frozen_string_literal: true

# The mutant gem's own mutations of Ruby files, localized and normalized so
# Supercov's enumeration (crates/supercov-engine/src/ruby_mutants.rs) can be
# compared with them entry by entry.
#
#   ruby scripts/mutant-parity.rb FILE...
#   ruby scripts/mutant-parity.rb --write-goldens FILE...
#
# Every `def` and `def self.x` in each file is a subject, as mutant treats a
# method it matches. Its mutations are mutant's (`Mutant::Mutator::Node`
# with the default configuration: light operators, no ignore patterns),
# kept where mutant keeps them (those Unparser round-trips), in mutant's
# order. Each is printed as:
#
#   { subject, start: [line, column], end: [line, column], node, replacement }
#
# A mutation replaces the whole method; it is localized to the smallest node
# whose replacement it is: from the method down, while exactly one child
# differs and that child is a node on both sides, into that child. A child
# that became nil in a body slot (a branch, a loop's or a block's body) is
# that child removed, and so is one statement dropped from a statement list.
# `node` is the localized node's type, `replacement` the replacing node as an
# S-expression (`null` where it was removed). Lines are 1-based, columns
# 1-based in characters.
#
# S-expressions are written so that Rust prints the same text: `(type child
# ...)`, a symbol as `:name`, a string as `s"<hex of its bytes>"`, an integer
# in decimal, a float as `f<hex of its IEEE bits>`, nil as `nil`, and any
# other value with `inspect`.
#
# Each subject also carries `sexp`, the method's own S-expression, so the
# conversion from Prism's tree can be checked node for node.
#
# --write-goldens writes FILE.mutant.json beside each FILE instead of
# printing. It needs the mutant gem (0.17.1) on Ruby 3.4.

require 'json'
require 'mutant'

MUTANT_SET = "mutant@#{Mutant::VERSION}"

# Body slots: a nil child there is an empty body, which a mutation may
# leave by removing what was there.
BODY_SLOTS = {
  if: [1, 2],
  while: [1],
  until: [1],
  block: [2],
  numblock: [2],
  itblock: [2],
  def: [2],
  defs: [3],
  in_pattern: [2],
  resbody: [2],
  ensure: [0, 1],
  case: [-1],
  case_match: [-1]
}.freeze

STATEMENT_LISTS = %i[begin kwbegin].freeze

def sexp(value)
  case value
  when Parser::AST::Node
    parts = value.children.map { |child| sexp(child) }
    "(#{[value.type, *parts].join(' ')})"
  when Symbol then ":#{value}"
  when String then "s\"#{value.b.unpack1('H*')}\""
  when Integer then value.to_s
  when Float then "f#{[value].pack('G').unpack1('H*')}"
  when nil then 'nil'
  else value.inspect
  end
end

def body_slot?(type, index, length)
  BODY_SLOTS.fetch(type, []).any? { |slot| (slot.negative? ? length + slot : slot) == index }
end

def position(range, start)
  if start
    [range.line, range.column + 1]
  else
    [range.last_line, range.last_column + 1]
  end
end

def localize(original, mutated)
  loop do
    if original.type == mutated.type && original.children.length == mutated.children.length
      differing = original.children.each_index.reject { |index| original.children[index] == mutated.children[index] }
      if differing.length == 1
        index = differing.first
        from = original.children[index]
        to = mutated.children[index]
        if from.is_a?(Parser::AST::Node) && to.is_a?(Parser::AST::Node)
          original = from
          mutated = to
          next
        end
        if from.is_a?(Parser::AST::Node) && to.nil? && body_slot?(original.type, index, original.children.length)
          return [from, nil]
        end
      end
    end
    if STATEMENT_LISTS.include?(original.type) && mutated.type == original.type &&
       mutated.children.length == original.children.length - 1
      index = original.children.each_index.find { |each| original.children[each] != mutated.children[each] }
      dropped = original.children.dup
      dropped.delete_at(index)
      return [original.children[index], nil] if dropped == mutated.children
    end
    return [original, mutated]
  end
end

def subjects(node, found = [])
  return found unless node.is_a?(Parser::AST::Node)

  found << node if %i[def defs].include?(node.type)
  node.children.each { |child| subjects(child, found) }
  found
end

def mutations(subject)
  config = Mutant::Mutation::Config::DEFAULT
  Mutant::Mutator::Node.mutate(config: config, node: subject).select do |mutated|
    Unparser.unparse_validate_ast_either(ast: Unparser::AST.from_node(node: mutated)).right?
  end
end

def entries(path)
  ast = Unparser.parse_ast_either(File.read(path)).from_right
  subjects(ast.node).map do |subject|
    name = subject.type == :def ? subject.children[0] : subject.children[1]
    {
      'subject' => "#{name}:#{subject.location.expression.line}",
      'sexp' => sexp(subject),
      'mutants' => mutations(subject).map do |mutated|
        node, replacement = localize(subject, mutated)
        range = node.location.expression
        {
          'start' => position(range, true),
          'end' => position(range, false),
          'node' => node.type.to_s,
          'replacement' => replacement && sexp(replacement)
        }
      end
    }
  end
end

write = ARGV.delete('--write-goldens')
ARGV.each do |path|
  document = { 'mutantSet' => MUTANT_SET, 'subjects' => entries(path) }
  if write
    File.write("#{path}.mutant.json", "#{JSON.pretty_generate(document)}\n")
    warn "#{path}: #{document['subjects'].sum { |subject| subject['mutants'].length }} mutants"
  else
    puts JSON.pretty_generate(document)
  end
end
