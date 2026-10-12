/**
 * A small, dependency-free JSON tree viewer.
 *
 * Renders nested objects/arrays with collapsible nodes and never throws: any
 * value (including `undefined`) renders safely, so a non-JSON body can't crash
 * the detail panel.
 */

import { useState } from "react";

type JsonValue =
  | null
  | boolean
  | number
  | string
  | JsonValue[]
  | { [key: string]: JsonValue };

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isContainer(value: unknown): value is JsonValue[] | Record<string, unknown> {
  return Array.isArray(value) || isPlainObject(value);
}

function primitive(value: unknown): string {
  if (value === null) return "null";
  if (value === undefined) return "undefined";
  if (typeof value === "string") return JSON.stringify(value);
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  if (typeof value === "bigint") return `${value.toString()}n`;
  if (typeof value === "function") return "ƒ ()";
  if (typeof value === "symbol") return value.toString();
  return String(value);
}

function primitiveClass(value: unknown): string {
  if (value === null || value === undefined) return "text-slate-500";
  if (typeof value === "string") return "text-amber-300";
  if (typeof value === "number") return "text-sky-300";
  if (typeof value === "boolean") return "text-fuchsia-300";
  return "text-slate-300";
}

function entries(value: JsonValue[] | Record<string, unknown>): [string, unknown][] {
  if (Array.isArray(value)) return value.map((item, index) => [String(index), item]);
  return Object.entries(value);
}

function CollapsibleNode({
  label,
  value,
  depth,
  defaultOpen,
}: {
  label: string;
  value: JsonValue[] | Record<string, unknown>;
  depth: number;
  defaultOpen: boolean;
}) {
  const [open, setOpen] = useState(defaultOpen);
  const isArray = Array.isArray(value);
  const childEntries = entries(value);
  const openBrace = isArray ? "[" : "{";
  const closeBrace = isArray ? "]" : "}";
  const typeLabel = isArray ? `Array(${childEntries.length})` : `Object(${childEntries.length})`;

  return (
    <div data-testid="json-node" className="leading-5" style={{ paddingLeft: depth === 0 ? 0 : 12 }}>
      <button
        type="button"
        className="group inline-flex items-center gap-1 text-left hover:bg-slate-800/60"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
      >
        <span className="w-3 text-slate-500">{open ? "▾" : "▸"}</span>
        {label !== "" && <span className="text-violet-300">{label}</span>}
        {label !== "" && <span className="text-slate-500">: </span>}
        <span className="text-slate-400">
          {openBrace}
          <span className="ml-1 text-[10px] uppercase tracking-wide text-slate-600">{typeLabel}</span>
        </span>
        {!open && <span className="text-slate-400">{closeBrace}</span>}
        {!open && childEntries.length > 0 && (
          <span className="text-slate-600">…</span>
        )}
      </button>
      {open && (
        <div>
          {childEntries.map(([key, child]) => (
            <JsonRow key={key} label={key} value={child} depth={depth + 1} />
          ))}
          <div style={{ paddingLeft: depth === 0 ? 12 : 24 }} className="text-slate-400">
            {closeBrace}
          </div>
        </div>
      )}
    </div>
  );
}

function JsonRow({ label, value, depth }: { label: string; value: unknown; depth: number }) {
  if (isContainer(value)) {
    return <CollapsibleNode label={label} value={value} depth={depth} defaultOpen={depth < 2} />;
  }
  return (
    <div data-testid="json-leaf" className="leading-5" style={{ paddingLeft: depth * 12 }}>
      <span className="text-violet-300">{label}</span>
      <span className="text-slate-500">: </span>
      <span className={primitiveClass(value)}>{primitive(value)}</span>
    </div>
  );
}

/** Render any JSON value as a tree. */
export function JsonTree({ value, rootLabel = "" }: { value: unknown; rootLabel?: string }) {
  if (!isContainer(value)) {
    return (
      <div className="font-mono text-xs" data-testid="json-tree">
        <span className={primitiveClass(value)}>{primitive(value)}</span>
      </div>
    );
  }
  return (
    <div className="font-mono text-xs" data-testid="json-tree">
      <CollapsibleNode label={rootLabel} value={value} depth={0} defaultOpen />
    </div>
  );
}
