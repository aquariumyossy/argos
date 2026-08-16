/** LLM mermaid often puts `[n]` cites inside `id[label]`, which ends the node early. */

const NODE_START = /^[A-Za-z][\w-]*/;

type QuotedNode = {
  start: number;
  end: number;
  id: string;
  label: string;
};

function isIdBoundary(ch: string | undefined): boolean {
  return !ch || !/[A-Za-z0-9_-]/.test(ch);
}

function findMatchingSquare(line: string, open: number): number {
  let depth = 1;
  for (let i = open + 1; i < line.length; i++) {
    const c = line[i];
    if (c === "[") depth += 1;
    else if (c === "]") {
      depth -= 1;
      if (depth === 0) return i;
    }
  }
  return -1;
}

function findQuotedSquareEnd(line: string, open: number): number {
  for (let i = open + 2; i < line.length - 1; i++) {
    if (line[i] === '"' && line[i + 1] === "]") return i + 1;
  }
  return -1;
}

function quotedNodesOnLine(line: string): QuotedNode[] {
  const out: QuotedNode[] = [];
  let i = 0;
  while (i < line.length) {
    if (!isIdBoundary(line[i - 1])) {
      i += 1;
      continue;
    }
    const rest = line.slice(i);
    const m = rest.match(NODE_START);
    if (!m || rest[m[0].length] !== "[" || rest[m[0].length + 1] !== '"') {
      i += 1;
      continue;
    }
    const id = m[0];
    const open = i + id.length;
    const close = findQuotedSquareEnd(line, open);
    if (close < 0) break;
    out.push({
      start: i,
      end: close + 1,
      id,
      label: line.slice(open + 2, close - 1),
    });
    i = close + 1;
  }
  return out;
}

function quoteSquareNodesOnLine(line: string): string {
  const trimmed = line.trimStart();
  if (trimmed.startsWith("%%") || trimmed.startsWith("subgraph ")) return line;

  let out = "";
  let i = 0;
  while (i < line.length) {
    if (!isIdBoundary(line[i - 1])) {
      out += line[i];
      i += 1;
      continue;
    }
    const rest = line.slice(i);
    const m = rest.match(NODE_START);
    if (!m || rest[m[0].length] !== "[") {
      out += line[i];
      i += 1;
      continue;
    }
    const id = m[0];
    const open = i + id.length;
    if (line[open + 1] === '"') {
      const close = findQuotedSquareEnd(line, open);
      if (close < 0) {
        out += line.slice(i);
        break;
      }
      out += line.slice(i, close + 1);
      i = close + 1;
      continue;
    }
    const close = findMatchingSquare(line, open);
    if (close < 0) {
      out += line.slice(i);
      break;
    }
    const inner = line.slice(open + 1, close);
    out += `${id}["${inner.replace(/"/g, "#quot;")}"]`;
    i = close + 1;
  }
  return out;
}

function fixBrokenArrows(src: string): string {
  return src
    .replace(/-\.\s*-+\s*>/g, "-.->")
    .replace(/-\.\s+>/g, "-.->")
    .replace(/--\s+>/g, "-->")
    .replace(/==\s+>/g, "==>");
}

function forceHorizontalFlowchart(src: string): string {
  return src.replace(/^(flowchart|graph)\s+(TB|TD|BT)\b/im, "$1 LR");
}

function isClaimCauseLabel(label: string): boolean {
  return /^\s*請求原因/.test(label);
}

function stripClaimPrefix(label: string): string {
  return label.replace(/^\s*請求原因[：:]\s*/, "").trim();
}

function replaceIdWord(src: string, from: string, to: string): string {
  const re = new RegExp(
    `\\b${from.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`,
    "g",
  );
  return src.replace(re, to);
}

function isSelfLoopLine(line: string): boolean {
  const m = line.match(
    /^\s*([A-Za-z][\w-]*)\s+(?:-->|---|-\.->|==>)\s*([A-Za-z][\w-]*)\s*$/,
  );
  return !!m && m[1] === m[2];
}

function isOnlyNodeDef(line: string, id: string): boolean {
  const nodes = quotedNodesOnLine(line);
  if (nodes.length !== 1 || nodes[0].id !== id) return false;
  return (
    line.slice(0, nodes[0].start).trim() === "" &&
    line.slice(nodes[0].end).trim() === ""
  );
}

/** Several 請求原因 boxes → one node (要件事実図). */
function mergeClaimCauseNodes(src: string): string {
  const lines = src.split("\n");
  const claims: { id: string; label: string }[] = [];
  const seen = new Set<string>();
  for (const line of lines) {
    for (const n of quotedNodesOnLine(line)) {
      if (!isClaimCauseLabel(n.label) || seen.has(n.id)) continue;
      seen.add(n.id);
      claims.push({ id: n.id, label: n.label });
    }
  }
  if (claims.length < 2) return src;

  const keep = claims[0];
  const extraIds = claims
    .slice(1)
    .map((c) => c.id)
    .sort((a, b) => b.length - a.length);
  const parts = claims.map((c) => stripClaimPrefix(c.label)).filter(Boolean);
  const mergedLabel = `請求原因：${parts.join(" / ")}`.replace(/"/g, "#quot;");

  let text = lines
    .filter((line) => !extraIds.some((id) => isOnlyNodeDef(line, id)))
    .join("\n");
  for (const id of extraIds) {
    text = replaceIdWord(text, id, keep.id);
  }

  const next = text.split("\n").filter((line) => !isSelfLoopLine(line));
  let replaced = false;
  return next
    .map((line) => {
      if (replaced) return line;
      for (const n of quotedNodesOnLine(line)) {
        if (n.id !== keep.id || !isClaimCauseLabel(n.label)) continue;
        replaced = true;
        return (
          line.slice(0, n.start) +
          `${keep.id}["${mergedLabel}"]` +
          line.slice(n.end)
        );
      }
      return line;
    })
    .join("\n");
}

export function sanitizeMermaidSource(src: string): string {
  const arrows = fixBrokenArrows(src.replace(/\r\n/g, "\n"));
  const quoted = arrows
    .split("\n")
    .map((line) => quoteSquareNodesOnLine(line))
    .join("\n");
  return mergeClaimCauseNodes(forceHorizontalFlowchart(quoted));
}
