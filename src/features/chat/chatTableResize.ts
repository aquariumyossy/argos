const MIN_COL_PX = 48;
/** Ignore 1–2px subpixel / border rounding so a near-fit table does not scroll. */
const OVERFLOW_EPS = 2;
const RESIZE_READY = "data-md-resize-ready";

function headerCells(table: HTMLTableElement): HTMLTableCellElement[] {
  const th = table.querySelectorAll("thead th");
  if (th.length > 0) return Array.from(th) as HTMLTableCellElement[];
  const row = table.rows[0];
  if (!row) return [];
  return Array.from(row.cells);
}

function wrapOf(table: HTMLTableElement): HTMLElement {
  return (
    (table.closest(".md-table-wrap") as HTMLElement | null) ??
    table.parentElement ??
    table
  );
}

function availableWidth(table: HTMLTableElement): number {
  return Math.max(0, Math.floor(wrapOf(table).clientWidth));
}

function ensureColgroup(table: HTMLTableElement, count: number): HTMLTableColElement[] {
  let group = table.querySelector("colgroup");
  if (!group) {
    group = document.createElement("colgroup");
    table.insertBefore(group, table.firstChild);
  }
  while (group.children.length < count) {
    group.appendChild(document.createElement("col"));
  }
  while (group.children.length > count) {
    group.lastElementChild?.remove();
  }
  return Array.from(group.querySelectorAll("col"));
}

function measureColWidths(table: HTMLTableElement, count: number): number[] {
  const row = table.tHead?.rows[0] ?? table.rows[0];
  if (!row) return Array(count).fill(120);
  const prevLayout = table.style.tableLayout;
  const prevWidth = table.style.width;
  table.style.width = "100%";
  table.style.tableLayout = "auto";
  const widths = Array.from(row.cells).map((cell) =>
    Math.max(MIN_COL_PX, cell.getBoundingClientRect().width),
  );
  table.style.tableLayout = prevLayout;
  table.style.width = prevWidth;
  while (widths.length < count) widths.push(120);
  return widths;
}

function scaleToFit(widths: number[], available: number): number[] {
  const n = widths.length;
  if (n === 0) return widths;
  const minTotal = n * MIN_COL_PX;
  const target = available < minTotal ? minTotal : available;
  const sum = widths.reduce((a, b) => a + b, 0) || 1;
  const floored = widths.map((w) =>
    Math.max(MIN_COL_PX, Math.floor((w / sum) * target)),
  );
  let rest = target - floored.reduce((a, b) => a + b, 0);
  for (let i = floored.length - 1; i >= 0 && rest !== 0; i--) {
    if (rest > 0) {
      floored[i] += rest;
      rest = 0;
    } else {
      const can = floored[i] - MIN_COL_PX;
      const take = Math.min(can, -rest);
      floored[i] -= take;
      rest += take;
    }
  }
  return floored;
}

function colPixelWidths(cols: HTMLTableColElement[]): number[] {
  return cols.map((col) => {
    const w = Number.parseFloat(col.style.width);
    if (Number.isFinite(w) && w > 0) return w;
    return Math.max(MIN_COL_PX, col.getBoundingClientRect().width);
  });
}

function applyWidths(
  table: HTMLTableElement,
  cols: HTMLTableColElement[],
  widths: number[],
): void {
  widths.forEach((w, i) => {
    cols[i].style.width = `${w}px`;
  });
  const sum = widths.reduce((a, b) => a + b, 0);
  const avail = availableWidth(table);
  table.style.tableLayout = "fixed";
  if (sum <= avail + OVERFLOW_EPS) {
    table.style.width = "100%";
  } else {
    table.style.width = `${sum}px`;
  }
  const wrap = wrapOf(table);
  wrap.classList.toggle("md-table-overflow", sum > avail + OVERFLOW_EPS);
}

function startResize(
  table: HTMLTableElement,
  colIndex: number,
  cols: HTMLTableColElement[],
  startX: number,
) {
  const startWidths = colPixelWidths(cols);
  const startW = startWidths[colIndex];
  const nextCol = cols[colIndex + 1];
  const startNextW = nextCol ? startWidths[colIndex + 1] : 0;

  const onMove = (ev: MouseEvent) => {
    const delta = ev.clientX - startX;
    const next = startWidths.slice();
    next[colIndex] = Math.max(MIN_COL_PX, startW + delta);
    if (nextCol && startNextW > 0) {
      next[colIndex + 1] = Math.max(MIN_COL_PX, startNextW - delta);
    }
    applyWidths(table, cols, next);
  };

  const onUp = () => {
    document.removeEventListener("mousemove", onMove);
    document.removeEventListener("mouseup", onUp);
    document.body.classList.remove("md-col-resizing");
  };

  document.body.classList.add("md-col-resizing");
  document.addEventListener("mousemove", onMove);
  document.addEventListener("mouseup", onUp);
}

function setupTable(table: HTMLTableElement): void {
  if (table.getAttribute(RESIZE_READY) === "1") return;
  const cells = headerCells(table);
  if (cells.length < 2) return;

  table.classList.add("md-table-resizable");

  const cols = ensureColgroup(table, cells.length);
  const fitted = scaleToFit(measureColWidths(table, cells.length), availableWidth(table));
  applyWidths(table, cols, fitted);

  cells.forEach((cell, i) => {
    if (i >= cells.length - 1) return;
    if (cell.querySelector(".md-col-resizer")) return;
    cell.classList.add("md-col-resize-cell");
    const handle = document.createElement("span");
    handle.className = "md-col-resizer";
    handle.setAttribute("role", "separator");
    handle.setAttribute("aria-orientation", "vertical");
    handle.title = "列幅をドラッグで調整";
    handle.addEventListener("mousedown", (e) => {
      e.preventDefault();
      e.stopPropagation();
      startResize(table, i, cols, e.clientX);
    });
    cell.appendChild(handle);
  });

  table.setAttribute(RESIZE_READY, "1");
}

/** Attach drag handles to multi-column tables inside a chat message body. */
export function attachResizableChatTables(root: HTMLElement | null): void {
  if (!root) return;
  for (const table of Array.from(
    root.querySelectorAll<HTMLTableElement>("table.md-table-resizable"),
  )) {
    setupTable(table);
  }
}
