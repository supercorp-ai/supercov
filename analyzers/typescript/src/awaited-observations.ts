import type ts from "typescript";
import type { SyntaxAPI } from "./frontend.js";

/** Source applicability only. This is not a runtime observation or a witness. */
export interface AwaitedObservationSource {
  model: "node-child-capture-poll-v1";
  factorySource: string;
  predicateSource: string;
  captures: { stream: "stdout" | "stderr"; source: string }[];
  pattern: string;
}

interface Context {
  syntax: SyntaxAPI;
  declaration(node: ts.Node): ts.Declaration | undefined;
  rawDeclaration(node: ts.Node): ts.Declaration | undefined;
  relativeFile(file: ts.SourceFile): string;
}

/**
 * Recognize a direct awaited factory method which polls an append-only native
 * child capture. Names, paths, regex and timeout values come from bindings/source.
 * Unsupported control flow or aliases are not evidence of a missing assertion.
 * Native APIs and absence of external monkey-patching remain model premises.
 */
export function awaitedObservationSource(
  context: Context,
  callback: ts.Node,
  call: ts.CallExpression,
): AwaitedObservationSource | undefined {
  const { syntax: t, declaration: decl, rawDeclaration: raw } = context;
  function require(value: unknown): asserts value {
    if (!value) throw unsupported;
  }
  const unsupported = Symbol("unsupported awaited observation");
  const nodes = (root: ts.Node, predicate: (node: ts.Node) => boolean) => {
    const found: ts.Node[] = [];
    const visit = (n: ts.Node) => {
      if (predicate(n)) found.push(n);
      t.forEachChild(n, visit);
    };
    visit(root);
    return found;
  };
  const one = <N extends ts.Node>(values: readonly N[]): N => {
    require(values.length === 1);
    return values[0];
  };
  const at = (node: ts.Node) => {
    const sf = node.getSourceFile(),
      p = sf.getLineAndCharacterOfPosition(node.getStart(sf));
    return `${context.relativeFile(sf)}:${p.line + 1}:${p.character + 1}`;
  };
  const importedFrom = (node: ts.Node, name: string): string | undefined => {
    const d = raw(node);
    return !!d &&
      t.isImportSpecifier(d) &&
      !d.isTypeOnly &&
      !d.parent.parent.isTypeOnly &&
      (d.propertyName ?? d.name).text === name &&
      t.isStringLiteral(d.parent.parent.parent.moduleSpecifier)
      ? d.parent.parent.parent.moduleSpecifier.text
      : undefined;
  };
  const constant = (node: ts.Node): node is ts.VariableDeclaration =>
    t.isVariableDeclaration(node) &&
    t.isIdentifier(node.name) &&
    t.isVariableDeclarationList(node.parent) &&
    !!(node.parent.flags & t.NodeFlags.Const) &&
    node.parent.declarations.length === 1;
  const simpleArrow = (node: ts.Node): node is ts.ArrowFunction =>
    t.isArrowFunction(node) &&
    node.parameters.length === 0 &&
    !node.modifiers?.length;
  const bodyExpression = (node: ts.ArrowFunction): ts.Expression => {
    if (!t.isBlock(node.body)) return node.body;
    require(node.body.statements.length === 1);
    const returned = node.body.statements[0];
    require(t.isReturnStatement(returned) && returned.expression);
    return returned.expression;
  };
  const positiveNumber = (node: ts.Node) =>
    t.isNumericLiteral(node) &&
    Number.isFinite(Number(node.text)) &&
    Number(node.text) > 0;
  const global = (node: ts.Node, name: string) => {
    const d = decl(node);
    return (
      t.isIdentifier(node) &&
      node.text === name &&
      (!d || d.getSourceFile().isDeclarationFile)
    );
  };
  const dateNow = (node: ts.Node) =>
    t.isCallExpression(node) &&
    !node.questionDotToken &&
    node.arguments.length === 0 &&
    t.isPropertyAccessExpression(node.expression) &&
    node.expression.name.text === "now" &&
    global(node.expression.expression, "Date");
  const references = (root: ts.Node, binding: ts.Declaration) =>
    nodes(root, (n) => t.isIdentifier(n) && decl(n) === binding);
  try {
    require(
      t.isArrowFunction(callback) &&
        t.isBlock(callback.body) &&
        callback.modifiers?.some((m) => m.kind === t.SyntaxKind.AsyncKeyword),
    );
    const registration = callback.parent;
    require(
      t.isCallExpression(registration) &&
        registration.arguments.at(-1) === callback &&
        importedFrom(registration.expression, "test") === "node:test" &&
        t.isExpressionStatement(registration.parent) &&
        t.isSourceFile(registration.parent.parent),
    );
    require(
      callback.body.statements.every(
        (s) => t.isVariableStatement(s) || t.isExpressionStatement(s),
      ),
    );
    require(
      t.isAwaitExpression(call.parent) &&
        t.isExpressionStatement(call.parent.parent) &&
        call.parent.parent.parent === callback.body &&
        call.arguments.length === 0 &&
        !call.questionDotToken &&
        t.isPropertyAccessExpression(call.expression) &&
        !call.expression.questionDotToken,
    );
    const member = call.expression.name.text,
      receiver = decl(call.expression.expression);
    require(
      receiver &&
        constant(receiver) &&
        receiver.initializer &&
        t.isCallExpression(receiver.initializer),
    );
    const launch = receiver.initializer,
      factory = decl(launch.expression);
    require(
      factory &&
        t.isFunctionDeclaration(factory) &&
        factory.body &&
        !factory.asteriskToken &&
        !factory.modifiers?.some((m) => m.kind === t.SyntaxKind.AsyncKeyword) &&
        factory.parameters.every(
          (p) => t.isIdentifier(p.name) && !p.initializer && !p.dotDotDotToken,
        ),
    );
    require(receiver.parent.parent.parent === callback.body);
    const statements: readonly ts.Statement[] = callback.body.statements;
    require(
      statements.indexOf(call.parent.parent) ===
        statements.indexOf(receiver.parent.parent as ts.Statement) + 1,
    );
    require(
      nodes(
        callback,
        (n) => t.isCallExpression(n) && decl(n.expression) === factory,
      ).length === 1,
    );
    require(
      nodes(
        callback,
        (n) =>
          t.isCallExpression(n) &&
          t.isPropertyAccessExpression(n.expression) &&
          decl(n.expression.expression) === receiver &&
          n.expression.name.text === member,
      ).length === 1,
    );
    require(
      nodes(
        callback,
        (n) => t.isIdentifier(n) && ["eval", "Function"].includes(n.text),
      ).length === 0,
    );

    const factoryBody = factory.body;
    require(
      factoryBody.statements.every(
        (s) =>
          t.isVariableStatement(s) ||
          t.isExpressionStatement(s) ||
          t.isReturnStatement(s),
      ),
    );
    const returned = one(factoryBody.statements.filter(t.isReturnStatement));
    require(
      returned === factoryBody.statements.at(-1) &&
        returned.expression &&
        t.isObjectLiteralExpression(returned.expression),
    );
    const members = returned.expression.properties;
    require(
      members.every(
        (p) =>
          (t.isPropertyAssignment(p) || t.isShorthandPropertyAssignment(p)) &&
          t.isIdentifier(p.name),
      ),
    );
    require(
      new Set(members.map((p) => p.name!.getText())).size === members.length,
    );
    const property = one(members.filter((p) => p.name?.getText() === member));
    require(
      t.isPropertyAssignment(property) && simpleArrow(property.initializer),
    );
    const pollCall = bodyExpression(property.initializer);
    require(
      t.isCallExpression(pollCall) &&
        !pollCall.questionDotToken &&
        pollCall.arguments.length >= 1,
    );
    const poll = decl(pollCall.expression);
    require(
      poll &&
        constant(poll) &&
        poll.parent.parent.parent === factoryBody &&
        poll.initializer &&
        t.isArrowFunction(poll.initializer) &&
        t.isBlock(poll.initializer.body) &&
        poll.initializer.modifiers?.some(
          (m) => m.kind === t.SyntaxKind.AsyncKeyword,
        ),
    );
    const pollFn = poll.initializer,
      pollBody = pollFn.body as ts.Block;
    require(
      pollFn.parameters.length === pollCall.arguments.length &&
        pollFn.parameters.every(
          (p) => t.isIdentifier(p.name) && !p.initializer && !p.dotDotDotToken,
        ),
    );
    require(
      pollBody.statements.length === 2 &&
        t.isVariableStatement(pollBody.statements[0]),
    );
    const deadline = one(pollBody.statements[0].declarationList.declarations);
    require(
      constant(deadline) &&
        deadline.initializer &&
        t.isBinaryExpression(deadline.initializer) &&
        deadline.initializer.operatorToken.kind === t.SyntaxKind.PlusToken &&
        dateNow(deadline.initializer.left) &&
        positiveNumber(deadline.initializer.right),
    );
    const loop = pollBody.statements[1];
    require(
      t.isWhileStatement(loop) &&
        t.isPrefixUnaryExpression(loop.expression) &&
        loop.expression.operator === t.SyntaxKind.ExclamationToken &&
        t.isCallExpression(loop.expression.operand),
    );
    const predicateCall = loop.expression.operand;
    require(
      predicateCall.arguments.length === 0 &&
        decl(predicateCall.expression) === pollFn.parameters[0] &&
        !predicateCall.questionDotToken &&
        t.isBlock(loop.statement) &&
        loop.statement.statements.length === 2,
    );
    require(
      references(pollFn, pollFn.parameters[0]).every(
        (n) =>
          n === pollFn.parameters[0].name || n === predicateCall.expression,
      ),
    );
    const [guard, pause] = loop.statement.statements;
    require(
      t.isIfStatement(guard) &&
        !guard.elseStatement &&
        t.isBlock(guard.thenStatement) &&
        guard.thenStatement.statements.length === 1 &&
        t.isThrowStatement(guard.thenStatement.statements[0]),
    );
    require(
      t.isExpressionStatement(pause) &&
        t.isAwaitExpression(pause.expression) &&
        t.isCallExpression(pause.expression.expression),
    );
    const delay = pause.expression.expression;
    require(
      importedFrom(delay.expression, "setTimeout") === "node:timers/promises" &&
        !delay.questionDotToken &&
        delay.arguments.length === 1 &&
        positiveNumber(delay.arguments[0]),
    );
    const predicate = pollCall.arguments[0];
    require(simpleArrow(predicate));
    const read = bodyExpression(predicate);
    require(
      t.isCallExpression(read) &&
        !read.questionDotToken &&
        read.arguments.length === 1 &&
        t.isPropertyAccessExpression(read.expression) &&
        read.expression.name.text === "test" &&
        t.isRegularExpressionLiteral(read.expression.expression),
    );
    const regex = read.expression.expression.text;
    require(!/[gy]/.test(regex.slice(regex.lastIndexOf("/") + 1)));
    const input = read.arguments[0];
    const operands =
      t.isBinaryExpression(input) &&
      input.operatorToken.kind === t.SyntaxKind.PlusToken
        ? [input.left, input.right]
        : [input];
    const buffers = operands.map((o) => decl(o));
    require(
      buffers.every((b) => b && t.isVariableDeclaration(b)) &&
        new Set(buffers).size === buffers.length,
    );
    let child: ts.VariableDeclaration | undefined;
    const captures: AwaitedObservationSource["captures"] = [];
    for (const [index, maybeBuffer] of buffers.entries()) {
      const buffer = maybeBuffer as ts.VariableDeclaration;
      require(
        buffer.parent.parent.parent === factoryBody &&
          t.isVariableDeclarationList(buffer.parent) &&
          !!(buffer.parent.flags & t.NodeFlags.Let) &&
          t.isIdentifier(buffer.name) &&
          buffer.initializer &&
          t.isStringLiteral(buffer.initializer) &&
          buffer.initializer.text === "",
      );
      const append = one(
        nodes(
          factoryBody,
          (n) =>
            t.isBinaryExpression(n) &&
            decl(n.left) === buffer &&
            n.operatorToken.kind >= t.SyntaxKind.FirstAssignment &&
            n.operatorToken.kind <= t.SyntaxKind.LastAssignment,
        ),
      );
      require(
        t.isBinaryExpression(append) &&
          append.operatorToken.kind === t.SyntaxKind.PlusEqualsToken &&
          t.isExpressionStatement(append.parent) &&
          t.isBlock(append.parent.parent),
      );
      const capture = append.parent.parent.parent;
      require(
        t.isArrowFunction(capture) &&
          !capture.modifiers?.length &&
          capture.parameters.length === 1 &&
          !capture.parameters[0].initializer &&
          !capture.parameters[0].dotDotDotToken &&
          decl(append.right) === capture.parameters[0] &&
          t.isBlock(capture.body) &&
          capture.body.statements.length === 1,
      );
      const on = capture.parent;
      require(
        t.isCallExpression(on) &&
          on.arguments.length === 2 &&
          on.arguments[1] === capture &&
          !on.questionDotToken &&
          t.isStringLiteral(on.arguments[0]) &&
          on.arguments[0].text === "data" &&
          t.isPropertyAccessExpression(on.expression) &&
          !on.expression.questionDotToken &&
          on.expression.name.text === "on" &&
          t.isCallExpression(on.expression.expression),
      );
      const encoding = on.expression.expression;
      require(
        !encoding.questionDotToken &&
          encoding.arguments.length === 1 &&
          t.isStringLiteral(encoding.arguments[0]) &&
          encoding.arguments[0].text === "utf8" &&
          t.isPropertyAccessExpression(encoding.expression) &&
          encoding.expression.name.text === "setEncoding" &&
          !encoding.expression.questionDotToken &&
          t.isPropertyAccessExpression(encoding.expression.expression),
      );
      const stream = encoding.expression.expression;
      require(
        !stream.questionDotToken &&
          (stream.name.text === "stdout" || stream.name.text === "stderr"),
      );
      const streamChild = decl(stream.expression);
      require(
        streamChild &&
          constant(streamChild) &&
          (!child || child === streamChild),
      );
      child = streamChild;
      require(
        t.isExpressionStatement(on.parent) && on.parent.parent === factoryBody,
      );
      // Only initialization, append, the predicate, trivial accessors and the
      // throw diagnostic can read this binding. No transformed/escaped capture.
      const allowed = new Set<ts.Node>([
        buffer.name,
        append.left,
        operands[index],
        ...references(guard.thenStatement, buffer),
      ]);
      for (const p of members)
        if (t.isPropertyAssignment(p) && simpleArrow(p.initializer)) {
          const value = bodyExpression(p.initializer);
          if (decl(value) === buffer) allowed.add(value);
        }
      require(references(factoryBody, buffer).every((n) => allowed.has(n)));
      const streamRefs = nodes(
        factoryBody,
        (n) =>
          t.isPropertyAccessExpression(n) &&
          decl(n.expression) === child &&
          n.name.text === stream.name.text,
      );
      require(streamRefs.length === 1);
      captures.push({ stream: stream.name.text, source: at(append) });
    }
    require(
      child &&
        child.initializer &&
        t.isCallExpression(child.initializer) &&
        child.parent.parent.parent === factoryBody &&
        importedFrom(child.initializer.expression, "spawn") ===
          "node:child_process",
    );
    const spawn = child.initializer;
    require(
      !spawn.questionDotToken &&
        spawn.arguments.length === 3 &&
        t.isObjectLiteralExpression(spawn.arguments[2]),
    );
    const options = spawn.arguments[2].properties;
    require(
      options.every(
        (p) => t.isPropertyAssignment(p) && t.isIdentifier(p.name),
      ) &&
        new Set(options.map((p) => p.name!.getText())).size === options.length,
    );
    const stdio = one(options.filter((p) => p.name?.getText() === "stdio"));
    require(
      t.isPropertyAssignment(stdio) &&
        t.isStringLiteral(stdio.initializer) &&
        stdio.initializer.text === "pipe",
    );
    for (const ref of references(factoryBody, child)) {
      if (
        ref === child.name ||
        (t.isShorthandPropertyAssignment(ref.parent) &&
          ref.parent.parent === returned.expression)
      )
        continue;
      require(
        t.isPropertyAccessExpression(ref.parent) &&
          ref.parent.expression === ref,
      );
      const access = ref.parent;
      require(
        [
          "stdout",
          "stderr",
          "exitCode",
          "signalCode",
          "pid",
          "once",
          "kill",
        ].includes(access.name.text),
      );
      require(
        !(
          t.isBinaryExpression(access.parent) &&
          access.parent.left === access &&
          access.parent.operatorToken.kind >= t.SyntaxKind.FirstAssignment &&
          access.parent.operatorToken.kind <= t.SyntaxKind.LastAssignment
        ) &&
          !t.isDeleteExpression(access.parent) &&
          !t.isPostfixUnaryExpression(access.parent) &&
          !(
            t.isPrefixUnaryExpression(access.parent) &&
            [t.SyntaxKind.PlusPlusToken, t.SyntaxKind.MinusMinusToken].includes(
              access.parent.operator,
            )
          ),
      );
      if (["once", "kill"].includes(access.name.text))
        require(
          t.isCallExpression(access.parent) &&
            access.parent.expression === access,
        );
    }
    const disjuncts = (n: ts.Expression): ts.Expression[] =>
      t.isBinaryExpression(n) &&
      n.operatorToken.kind === t.SyntaxKind.BarBarToken
        ? [...disjuncts(n.left), ...disjuncts(n.right)]
        : [n];
    const checks = disjuncts(guard.expression);
    require(checks.length === 3);
    for (const [index, name] of ["exitCode", "signalCode"].entries()) {
      const check = checks[index];
      require(
        t.isBinaryExpression(check) &&
          check.operatorToken.kind ===
            t.SyntaxKind.ExclamationEqualsEqualsToken &&
          check.right.kind === t.SyntaxKind.NullKeyword &&
          t.isPropertyAccessExpression(check.left) &&
          check.left.name.text === name &&
          decl(check.left.expression) === child,
      );
    }
    const expired = checks[2];
    require(
      t.isBinaryExpression(expired) &&
        expired.operatorToken.kind === t.SyntaxKind.GreaterThanToken &&
        dateNow(expired.left) &&
        decl(expired.right) === deadline,
    );
    require(
      references(factoryBody, poll).every(
        (n) =>
          n === poll.name ||
          n === pollCall.expression ||
          (t.isShorthandPropertyAssignment(n.parent) &&
            n.parent.parent === returned.expression),
      ),
    );
    for (const ref of references(callback, receiver)) {
      if (ref === receiver.name) continue;
      require(
        t.isPropertyAccessExpression(ref.parent) &&
          ref.parent.expression === ref,
      );
      const use = ref.parent;
      require(t.isCallExpression(use.parent) && use.parent.expression === use);
      const usedProperty = one(
        members.filter((p) => p.name?.getText() === use.name.text),
      );
      require(
        usedProperty === property ||
          (t.isShorthandPropertyAssignment(usedProperty) &&
            decl(usedProperty.name) === poll) ||
          (t.isPropertyAssignment(usedProperty) &&
            simpleArrow(usedProperty.initializer) &&
            buffers.includes(decl(bodyExpression(usedProperty.initializer)))),
      );
    }
    require(
      nodes(
        factoryBody,
        (n) => t.isIdentifier(n) && ["eval", "Function"].includes(n.text),
      ).length === 0,
    );
    return {
      model: "node-child-capture-poll-v1",
      factorySource: at(factory),
      predicateSource: at(read),
      captures,
      pattern: regex,
    };
  } catch (error) {
    if (error === unsupported) return undefined;
    throw error;
  }
}
