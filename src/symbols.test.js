import { describe, it, expect } from "vitest";
import { javascriptLanguage } from "@codemirror/lang-javascript";
import { pythonLanguage } from "@codemirror/lang-python";
import { markdownLanguage } from "@codemirror/lang-markdown";
import { extractSymbols, normalizeLspSymbols } from "./symbols.js";

const symbolsOf = (lang, code) => extractSymbols(lang.parser.parse(code), code);
const names = (syms) => syms.map((s) => s.name);

describe("extractSymbols", () => {
  it("returns nothing for code with no definitions", () => {
    expect(symbolsOf(javascriptLanguage, "const a = 1;\nconsole.log(a);\n")).toEqual([]);
  });

  it("extracts JS functions, classes, and methods", () => {
    const code = [
      "function alpha() {}",
      "class Beta {",
      "  gamma() {}",
      "  delta() {}",
      "}",
      "function epsilon(x) { return x; }",
    ].join("\n");
    const syms = symbolsOf(javascriptLanguage, code);
    expect(names(syms)).toEqual(["alpha", "Beta", "gamma", "delta", "epsilon"]);
    const beta = syms.find((s) => s.name === "Beta");
    expect(beta.kind).toBe("class");
    expect(syms.find((s) => s.name === "gamma").kind).toBe("method");
  });

  it("orders symbols by position and records a jump offset", () => {
    const code = "function one() {}\nfunction two() {}\n";
    const syms = symbolsOf(javascriptLanguage, code);
    expect(syms[0].from).toBeLessThan(syms[1].from);
    expect(code.slice(syms[1].from)).toMatch(/^function two/);
  });

  it("extracts Python defs and classes", () => {
    const code = ["def top():", "    pass", "", "class Widget:", "    def render(self):", "        pass"].join("\n");
    const syms = symbolsOf(pythonLanguage, code);
    expect(names(syms)).toContain("top");
    expect(names(syms)).toContain("Widget");
    expect(names(syms)).toContain("render");
  });

  it("extracts Markdown headings with levels, stripping the hashes", () => {
    const code = "# Title\n\nsome text\n\n## Section\n\n### Sub\n";
    const syms = symbolsOf(markdownLanguage, code);
    expect(names(syms)).toEqual(["Title", "Section", "Sub"]);
    expect(syms.map((s) => s.level)).toEqual([1, 2, 3]);
    expect(syms.every((s) => s.kind === "heading")).toBe(true);
  });
});

describe("normalizeLspSymbols", () => {
  it("returns [] for non-array / empty input", () => {
    expect(normalizeLspSymbols(null)).toEqual([]);
    expect(normalizeLspSymbols(undefined)).toEqual([]);
    expect(normalizeLspSymbols([])).toEqual([]);
  });

  it("flattens hierarchical DocumentSymbol[] with nesting levels", () => {
    // class Foo { bar() {} } — DocumentSymbol form (selectionRange + children).
    const result = [
      {
        name: "Foo",
        kind: 5, // Class
        range: { start: { line: 0, character: 0 }, end: { line: 2, character: 1 } },
        selectionRange: { start: { line: 0, character: 6 }, end: { line: 0, character: 9 } },
        children: [
          {
            name: "bar",
            kind: 6, // Method
            range: { start: { line: 1, character: 2 }, end: { line: 1, character: 12 } },
            selectionRange: { start: { line: 1, character: 2 }, end: { line: 1, character: 5 } },
          },
        ],
      },
    ];
    const syms = normalizeLspSymbols(result);
    expect(syms).toEqual([
      { name: "Foo", kind: "class", line: 0, character: 6, level: 0 },
      { name: "bar", kind: "method", line: 1, character: 2, level: 1 },
    ]);
  });

  it("handles flat SymbolInformation[] (location-based)", () => {
    const result = [
      {
        name: "topLevel",
        kind: 12, // Function
        location: { uri: "file:///x.ts", range: { start: { line: 4, character: 0 }, end: { line: 4, character: 8 } } },
      },
    ];
    const syms = normalizeLspSymbols(result);
    expect(syms).toEqual([{ name: "topLevel", kind: "function", line: 4, character: 0, level: 0 }]);
  });

  it("falls back to 'symbol' for unmapped kinds", () => {
    const result = [
      { name: "weird", kind: 99, selectionRange: { start: { line: 0, character: 0 } } },
    ];
    expect(normalizeLspSymbols(result)[0].kind).toBe("symbol");
  });
});
