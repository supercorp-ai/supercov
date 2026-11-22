// Command supercov-mutants answers, for each Go mutant Supercov lists, the
// questions a strong pass needs answered before it builds anything: does the
// program still compile with the mutant in place, and is the mutated
// expression a constant.
//
// It reads a request on stdin and writes its answer on stdout, both JSON. It
// loads the module's packages the way `go vet` does -- `go list -export` for
// every dependency's export data, go/types for the package itself -- so it
// needs nothing beyond the standard library and the toolchain the module is
// built with.
//
// A mutant is checked by swapping its token in the package's syntax tree
// (Gremlins swaps the same token) and type-checking the package again, with
// the package's own `_test.go` files, as `go test` compiles it: an unused
// variable, a constant that overflows, a `continue` outside a loop, an
// operator the operand's type does not have -- whatever the compiler
// refuses, go/types refuses here. Only mutants whose viability no rule
// settles are checked again; the rest are viable by the rule.
package main

import (
	"bytes"
	"encoding/json"
	"fmt"
	"go/ast"
	"go/importer"
	"go/parser"
	"go/token"
	"go/types"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"sync"
)

type request struct {
	// The directory `go list` runs in: the module root.
	Dir string `json:"dir"`
	// Patterns for `go list`, `./a/b` for each package directory a mutant
	// is in.
	Patterns []string `json:"patterns"`
	// Build tags, as `-tags` takes them.
	Tags string `json:"tags"`
	// The go command to list with: the one the project's tests run.
	Go      string   `json:"go"`
	Mutants []mutant `json:"mutants"`
}

type mutant struct {
	// Relative to Dir, `/`-separated.
	File string `json:"file"`
	// Byte offset of the mutated token.
	Offset int `json:"offset"`
	// The token it becomes: `<=`, `-`, `continue`, `=`.
	New string `json:"new"`
}

type answer struct {
	// One per requested mutant, in order.
	Mutants []verdict `json:"mutants"`
	// Packages that could not be checked, with why.
	Unchecked map[string]string `json:"unchecked,omitempty"`
}

type verdict struct {
	// "viable", "unviable", or "unchecked".
	Status string `json:"status"`
	// The compiler's refusal, for an unviable mutant.
	Error string `json:"error,omitempty"`
	// Whether the expression the token is the operator of is a constant: a
	// switch cannot be written into it without changing how the compiler
	// evaluates the original.
	Constant bool `json:"constant,omitempty"`
	// Whether a constant is required where it stands (a constant
	// declaration, an array length): no switch can stand there.
	ConstantContext bool `json:"constantContext,omitempty"`
	// The operands' type is a floating-point or complex one: NaN orders as
	// nothing, so `!(a < b)` is not `a >= b`.
	Float bool `json:"float,omitempty"`
	// The operands' type is a string one.
	String bool `json:"string,omitempty"`
	// The expression's own type, where it is typed and named in a way the
	// file can write (`int64`, `time.Duration`): what a switch over two
	// untyped constants converts to.
	Type string `json:"type,omitempty"`
	// For a constant, the bytes of the largest constant expression around
	// it: a switch holds it whole, so the compiler still evaluates every
	// alternative as a constant.
	Root *[2]int `json:"root,omitempty"`
	// That expression's type, written as Type is, and whether it is a
	// boolean.
	RootType string `json:"rootType,omitempty"`
	RootBool bool   `json:"rootBool,omitempty"`
}

type listed struct {
	ImportPath      string
	Name            string
	Dir             string
	GoFiles         []string
	CgoFiles        []string
	TestGoFiles     []string
	Export          string
	ForTest         string
	DepOnly         bool
	Standard        bool
	ImportMap       map[string]string
	Module          *struct{ GoVersion string }
	Error           *struct{ Err string }
	Incomplete      bool
	CompiledGoFiles []string
}

func main() {
	var req request
	if err := json.NewDecoder(os.Stdin).Decode(&req); err != nil {
		fail(err)
	}
	out, err := run(req)
	if err != nil {
		fail(err)
	}
	if err := json.NewEncoder(os.Stdout).Encode(out); err != nil {
		fail(err)
	}
}

func fail(err error) {
	fmt.Fprintln(os.Stderr, "supercov-mutants:", err)
	os.Exit(2)
}

func run(req request) (answer, error) {
	packages, err := list(req)
	if err != nil {
		return answer{}, err
	}
	exports := map[string]string{}
	for _, p := range packages {
		if p.Export != "" {
			exports[p.ImportPath] = p.Export
		}
	}
	// The package each mutant's file is compiled in: the test variant where
	// the package has internal tests, since that is what `go test` builds.
	byFile := map[string]*listed{}
	for index := range packages {
		p := &packages[index]
		if p.DepOnly || p.Standard || strings.HasSuffix(p.ImportPath, ".test") {
			continue
		}
		if strings.HasSuffix(p.Name, "_test") {
			continue
		}
		for _, file := range append(append([]string{}, p.GoFiles...), p.CgoFiles...) {
			if strings.HasSuffix(file, "_test.go") {
				continue
			}
			full := filepath.Join(p.Dir, file)
			if seen, ok := byFile[full]; ok && seen.ForTest != "" && p.ForTest == "" {
				continue
			}
			byFile[full] = p
		}
	}
	out := answer{Mutants: make([]verdict, len(req.Mutants)), Unchecked: map[string]string{}}
	groups := map[*listed][]int{}
	for index, m := range req.Mutants {
		full := filepath.Join(req.Dir, filepath.FromSlash(m.File))
		p, ok := byFile[full]
		if !ok {
			// Not compiled in this configuration (another GOOS, a build tag):
			// no test can run it, and nothing here can check it.
			out.Mutants[index] = verdict{Status: "unchecked", Error: "not in any package this build compiles"}
			continue
		}
		groups[p] = append(groups[p], index)
	}
	var mu sync.Mutex
	var wait sync.WaitGroup
	limit := make(chan struct{}, runtime.GOMAXPROCS(0))
	for p, indices := range groups {
		wait.Add(1)
		go func(p *listed, indices []int) {
			defer wait.Done()
			limit <- struct{}{}
			defer func() { <-limit }()
			verdicts, why := checkPackage(req, p, exports, indices)
			mu.Lock()
			defer mu.Unlock()
			if why != "" {
				out.Unchecked[p.ImportPath] = why
			}
			for i, index := range indices {
				out.Mutants[index] = verdicts[i]
			}
		}(p, indices)
	}
	wait.Wait()
	return out, nil
}

func list(req request) ([]listed, error) {
	args := []string{"list", "-e", "-test", "-deps", "-export", "-json"}
	if req.Tags != "" {
		args = append(args, "-tags", req.Tags)
	}
	args = append(args, req.Patterns...)
	program := req.Go
	if program == "" {
		program = "go"
	}
	command := exec.Command(program, args...)
	command.Dir = req.Dir
	var stderr bytes.Buffer
	command.Stderr = &stderr
	stdout, err := command.Output()
	if err != nil && len(stdout) == 0 {
		return nil, fmt.Errorf("go list: %v: %s", err, stderr.String())
	}
	var packages []listed
	decoder := json.NewDecoder(bytes.NewReader(stdout))
	for {
		var p listed
		if err := decoder.Decode(&p); err == io.EOF {
			break
		} else if err != nil {
			return nil, err
		}
		packages = append(packages, p)
	}
	return packages, nil
}

// The files of one package as parsed, and where each requested mutant's
// token is in them.
type loaded struct {
	fset  *token.FileSet
	files []*ast.File
	// By file path, the parsed file and its token.File.
	byPath map[string]*ast.File
}

func parse(p *listed) (*loaded, error) {
	l := &loaded{fset: token.NewFileSet(), byPath: map[string]*ast.File{}}
	names := append(append([]string{}, p.GoFiles...), p.CgoFiles...)
	sort.Strings(names)
	for _, name := range names {
		full := name
		if !filepath.IsAbs(full) {
			full = filepath.Join(p.Dir, name)
		}
		file, err := parser.ParseFile(l.fset, full, nil, parser.SkipObjectResolution|parser.ParseComments)
		if err != nil {
			return nil, err
		}
		l.files = append(l.files, file)
		l.byPath[full] = file
		fsetLock.Lock()
		fsetOf[file] = l.fset
		fsetLock.Unlock()
	}
	return l, nil
}

// The importer a package is checked with, every time: it keeps what it read
// of each dependency's export data.
func importerFor(p *listed, exports map[string]string, fset *token.FileSet) types.Importer {
	lookup := func(path string) (io.ReadCloser, error) {
		if mapped, ok := p.ImportMap[path]; ok {
			path = mapped
		}
		file, ok := exports[path]
		if !ok {
			return nil, fmt.Errorf("no export data for %s", path)
		}
		return os.Open(file)
	}
	return importer.ForCompiler(fset, "gc", lookup)
}

func config(p *listed, imports types.Importer, errors *[]types.Error) *types.Config {
	c := &types.Config{
		Importer:    imports,
		FakeImportC: true,
		Error: func(err error) {
			if typed, ok := err.(types.Error); ok {
				*errors = append(*errors, typed)
			}
		},
	}
	if p.Module != nil && p.Module.GoVersion != "" {
		c.GoVersion = "go" + p.Module.GoVersion
	}
	return c
}

// A mutated token's node: where Gremlins finds it, and how it is swapped.
type site struct {
	node ast.Node
	tok  *token.Token
}

func sites(file *ast.File) map[token.Pos]site {
	found := map[token.Pos]site{}
	ast.Inspect(file, func(node ast.Node) bool {
		switch n := node.(type) {
		case *ast.AssignStmt:
			found[n.TokPos] = site{n, &n.Tok}
		case *ast.BinaryExpr:
			found[n.OpPos] = site{n, &n.Op}
		case *ast.BranchStmt:
			found[n.TokPos] = site{n, &n.Tok}
		case *ast.IncDecStmt:
			found[n.TokPos] = site{n, &n.Tok}
		case *ast.UnaryExpr:
			found[n.OpPos] = site{n, &n.Op}
		}
		return true
	})
	return found
}

var tokens = func() map[string]token.Token {
	byText := map[string]token.Token{}
	for t := token.ILLEGAL; t <= token.TILDE; t++ {
		if t.IsOperator() || t.IsKeyword() {
			byText[t.String()] = t
		}
	}
	return byText
}()

func checkPackage(req request, p *listed, exports map[string]string, indices []int) ([]verdict, string) {
	verdicts := make([]verdict, len(indices))
	unchecked := func(why string) ([]verdict, string) {
		for i := range verdicts {
			verdicts[i] = verdict{Status: "unchecked", Error: why}
		}
		return verdicts, why
	}
	l, err := parse(p)
	if err != nil {
		return unchecked(err.Error())
	}
	imports := importerFor(p, exports, l.fset)
	var errors []types.Error
	info := &types.Info{Types: map[ast.Expr]types.TypeAndValue{}}
	pkg, _ := config(p, imports, &errors).Check(p.ImportPath, l.fset, l.files, info)
	if len(errors) > 0 || pkg == nil {
		// The package as written does not check here (cgo, an unusual
		// toolchain): nothing can be said of its mutants.
		return unchecked(fmt.Sprintf("the package as written does not type-check here: %v", errors[0]))
	}
	parents := map[ast.Node]ast.Node{}
	for _, file := range l.files {
		var stack []ast.Node
		ast.Inspect(file, func(node ast.Node) bool {
			if node == nil {
				stack = stack[:len(stack)-1]
				return true
			}
			if len(stack) > 0 {
				parents[node] = stack[len(stack)-1]
			}
			stack = append(stack, node)
			return true
		})
	}
	bySite := map[string]map[token.Pos]site{}
	for path, file := range l.byPath {
		bySite[path] = sites(file)
	}
	for i, index := range indices {
		m := req.Mutants[index]
		full := filepath.Join(req.Dir, filepath.FromSlash(m.File))
		file, ok := l.byPath[full]
		if !ok {
			verdicts[i] = verdict{Status: "unchecked", Error: "file not in its package's build"}
			continue
		}
		tf := l.fset.File(file.Pos())
		if m.Offset < 0 || m.Offset >= tf.Size() {
			verdicts[i] = verdict{Status: "unchecked", Error: "offset out of the file"}
			continue
		}
		s, ok := bySite[full][tf.Pos(m.Offset)]
		replacement, known := tokens[m.New]
		if !ok || !known {
			verdicts[i] = verdict{Status: "unchecked", Error: "no such token there"}
			continue
		}
		v := facts(s, info, parents, pkg, file)
		if needsCheck(s, v, replacement, info) {
			original := *s.tok
			*s.tok = replacement
			var mutatedErrors []types.Error
			config(p, imports, &mutatedErrors).Check(p.ImportPath, l.fset, l.files, &types.Info{})
			*s.tok = original
			if len(mutatedErrors) > 0 {
				first := mutatedErrors[0]
				position := l.fset.Position(first.Pos)
				v.Status = "unviable"
				v.Error = fmt.Sprintf("%s:%d:%d: %s", filepath.Base(position.Filename), position.Line, position.Column, first.Msg)
				verdicts[i] = v
				continue
			}
		}
		v.Status = "viable"
		verdicts[i] = v
	}
	return verdicts, ""
}

// What the original program says of the mutated expression.
// The file set each parsed file belongs to.
var (
	fsetOf   = map[*ast.File]*token.FileSet{}
	fsetLock sync.Mutex
)

func facts(s site, info *types.Info, parents map[ast.Node]ast.Node, pkg *types.Package, file *ast.File) verdict {
	var v verdict
	var expr ast.Expr
	var operand ast.Expr
	switch n := s.node.(type) {
	case *ast.BinaryExpr:
		expr, operand = n, n.X
	case *ast.UnaryExpr:
		expr, operand = n, n.X
	case *ast.AssignStmt:
		if len(n.Lhs) > 0 {
			operand = n.Lhs[0]
		}
	case *ast.IncDecStmt:
		operand = n.X
	}
	if operand != nil {
		if tv, ok := info.Types[operand]; ok && tv.Type != nil {
			if basic, ok := tv.Type.Underlying().(*types.Basic); ok {
				v.Float = basic.Info()&(types.IsFloat|types.IsComplex) != 0
				v.String = basic.Info()&types.IsString != 0
			}
		}
	}
	if expr != nil {
		if tv, ok := info.Types[expr]; ok {
			v.Constant = tv.Value != nil
			if tv.Type != nil {
				if basic, ok := tv.Type.(*types.Basic); !ok || basic.Info()&types.IsUntyped == 0 {
					v.Type = types.TypeString(tv.Type, qualifier(pkg, file))
				}
			}
			if v.Constant {
				v.ConstantContext = requiresConstant(expr, info, parents)
				root := largestConstant(expr, info, parents)
				fsetLock.Lock()
				fset := fsetOf[file]
				fsetLock.Unlock()
				v.Root = &[2]int{fset.Position(root.Pos()).Offset, fset.Position(root.End()).Offset}
				if rtv := info.Types[root]; rtv.Type != nil {
					if basic, ok := rtv.Type.Underlying().(*types.Basic); ok {
						v.RootBool = basic.Info()&types.IsBoolean != 0
					}
					if basic, ok := rtv.Type.(*types.Basic); !ok || basic.Info()&types.IsUntyped == 0 {
						v.RootType = types.TypeString(rtv.Type, qualifier(pkg, file))
					}
				}
			}
		}
	}
	return v
}

// The outermost expression around `expr` that is still a constant.
func largestConstant(expr ast.Expr, info *types.Info, parents map[ast.Node]ast.Node) ast.Expr {
	for {
		parent, ok := parents[expr].(ast.Expr)
		if !ok {
			return expr
		}
		if tv, known := info.Types[parent]; !known || tv.Value == nil {
			if _, paren := parent.(*ast.ParenExpr); !paren {
				return expr
			}
		}
		expr = parent
	}
}

// How a type from another package is written in this file: by the name the
// file imports its package under. A package the file does not import cannot
// be written, and its type is left unwritten.
func qualifier(pkg *types.Package, file *ast.File) types.Qualifier {
	names := map[string]string{}
	for _, spec := range file.Imports {
		path := strings.Trim(spec.Path.Value, "\"`")
		name := ""
		if spec.Name != nil {
			name = spec.Name.Name
		}
		names[path] = name
	}
	return func(other *types.Package) string {
		if other == pkg {
			return ""
		}
		name, ok := names[other.Path()]
		if !ok || name == "_" || name == "." {
			return "\x00"
		}
		if name == "" {
			return other.Name()
		}
		return name
	}
}

// Whether the outermost constant expression around `expr` stands where Go
// requires a constant: a constant declaration's value, an array type's
// length, or the operand of a constant conversion to a constant's type in
// one of those.
func requiresConstant(expr ast.Expr, info *types.Info, parents map[ast.Node]ast.Node) bool {
	var node ast.Node = expr
	for {
		parent := parents[node]
		switch p := parent.(type) {
		case nil:
			return false
		case *ast.ValueSpec:
			// A `const` spec's values; a `var` spec's are values like any.
			if decl, ok := parents[p].(*ast.GenDecl); ok && decl.Tok == token.CONST {
				return true
			}
			return false
		case *ast.ArrayType:
			return p.Len == node
		case *ast.Ellipsis:
			return false
		case ast.Expr:
			// Still inside an expression: keep going while it is constant, or
			// a type expression around an array length.
			if tv, ok := info.Types[p]; ok && (tv.Value != nil || tv.IsType()) {
				node = parent
				continue
			}
			if _, ok := p.(*ast.ParenExpr); ok {
				node = parent
				continue
			}
			return false
		case *ast.Field, *ast.TypeSpec:
			// Inside a type: an array length in a struct field's type, say.
			node = parent
			continue
		default:
			return false
		}
	}
}

// Whether no rule settles the mutant's viability, so the package has to be
// checked again with it in place. A rule here is only ever a reason to
// skip a check that would pass: where in doubt, check.
func needsCheck(s site, v verdict, replacement token.Token, info *types.Info) bool {
	constantOperand := func(expr ast.Expr) bool {
		tv, ok := info.Types[expr]
		return !ok || tv.Value != nil
	}
	switch n := s.node.(type) {
	case *ast.BinaryExpr:
		if v.Constant {
			// Overflow, division by zero, a negative shift: constants are
			// evaluated by the compiler.
			return true
		}
		switch replacement {
		case token.EQL, token.NEQ, token.LSS, token.LEQ, token.GTR, token.GEQ, token.LAND, token.LOR:
			// A comparison stays a comparison of the same operands, and
			// `&&` stays a logical operator of booleans.
			return false
		case token.ADD, token.SUB, token.MUL:
			// Defined on every numeric type; `+` of strings has no `-`.
			return v.String
		case token.QUO, token.REM:
			// A constant zero divisor is refused.
			return v.String || constantOperand(n.Y)
		}
		return true
	case *ast.UnaryExpr:
		// `-x` to `+x` keeps the type; a constant may overflow its type.
		return v.Constant || replacement != token.ADD
	case *ast.IncDecStmt:
		return false
	}
	// Assignments (a self-assignment removed can leave a variable unused,
	// a shift's count is another type), `break` and `continue` (a loop to
	// continue, a label's statement): checked.
	return true
}
