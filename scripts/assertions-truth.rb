# assertions-truth.rb <file> <line> <change>: the file's versions with the
# statement at <line> changed as the assessment's question describes, as a
# JSON array of whole texts. Read on Prism's tree, as Supercov reads Ruby.
# Used by assertions-truth.mjs.
require "prism"
require "json"

file, line, change = ARGV[0], Integer(ARGV[1]), ARGV[2]
source = File.read(file, encoding: "UTF-8")

# The nodes of statement lists that start on the line, outermost first, and
# the `if`, `unless` and `elsif` whose keyword is on it.
class Finder < Prism::Visitor
  attr_reader :statements, :conditions

  def initialize(line)
    @line = line
    @statements = []
    @conditions = []
  end

  def visit_statements_node(node)
    node.body.each { |child| @statements << child if child.location.start_line == @line }
    super
  end

  def visit_if_node(node)
    keyword = node.if_keyword_loc
    @conditions << node if keyword && keyword.start_line == @line
    super
  end

  def visit_unless_node(node)
    @conditions << node if node.keyword_loc.start_line == @line
    super
  end
end

finder = Finder.new(line)
Prism.parse(source).value.accept(finder)

def replace(source, location, text)
  bytes = source.b
  (bytes[0...location.start_offset] + text.b + bytes[location.end_offset..]).force_encoding(source.encoding)
end

node = finder.statements.first
versions = []
case change
when "condition inverted"
  # Taken as false whenever it is true, and as true whenever it is false: the
  # condition still runs.
  if (condition = finder.conditions.first)
    predicate = condition.predicate
    versions << replace(source, predicate.location, "((#{predicate.slice}) && false)")
    versions << replace(source, predicate.location, "((#{predicate.slice}) || true)")
  end
when "skipped"
  versions << replace(source, node.location, "nil") if node
when "returns nil"
  if node.is_a?(Prism::ReturnNode)
    versions << replace(source, node.location, "return nil")
  elsif node
    versions << replace(source, node.location, "nil")
  end
when "value becomes nil"
  versions << replace(source, node.value.location, "nil") if node.respond_to?(:value) && node.value
end
puts JSON.generate(versions)
