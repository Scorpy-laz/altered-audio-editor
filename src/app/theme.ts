// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

/**
 * Theme colors for canvas drawing. Canvases cannot use CSS variables
 * directly, so they read them here. Values are cached, and the cache is
 * dropped whenever the root element's surface (Standard, Classic, Light)
 * or its inline color overrides change. `themeTick` counts those changes,
 * so a drawing effect that reads it redraws at once.
 */

import { readable } from "svelte/store";

const cache = new Map<string, string>();
let style: CSSStyleDeclaration | null = null;

/** The value of a root CSS variable, trimmed; `fallback` when it is not set. */
export function cssVar(name: string, fallback = ""): string {
  let v = cache.get(name);
  if (v === undefined) {
    if (typeof document === "undefined") return fallback;
    style ??= getComputedStyle(document.documentElement);
    v = style.getPropertyValue(name).trim();
    cache.set(name, v);
  }
  return v || fallback;
}

/** A root CSS variable read as a number (for alphas and amounts). */
export function cssNum(name: string, fallback: number): number {
  const n = Number.parseFloat(cssVar(name));
  return Number.isFinite(n) ? n : fallback;
}

/** Drops the cached values. Called when the theme changes. */
export function invalidateTheme(): void {
  cache.clear();
  style = null;
}

/** Counts theme changes. Read it in a drawing effect to redraw when the theme changes. */
export const themeTick = readable(0, (set) => {
  if (typeof document === "undefined" || typeof MutationObserver === "undefined") return;
  let n = 0;
  const obs = new MutationObserver(() => {
    invalidateTheme();
    set(++n);
  });
  obs.observe(document.documentElement, { attributes: true, attributeFilter: ["data-surface", "style"] });
  return () => obs.disconnect();
});

/** Whether the light surface is on. */
export function isLightSurface(): boolean {
  return typeof document !== "undefined" && document.documentElement.dataset.surface === "light";
}

/** Mixes a hex or rgb() color toward `target` (a hex or rgb() color) by `amount` (0 to 1). Unknown formats come back unchanged. */
export function mixColor(c: string, target: string, amount: number): string {
  const a = parseRgb(c);
  const b = parseRgb(target);
  if (!a || !b) return c;
  const mix = (i: number) => Math.round(a[i]! + (b[i]! - a[i]!) * amount);
  return `rgb(${mix(0)}, ${mix(1)}, ${mix(2)})`;
}

/** Reads `#rgb`, `#rrggbb` or `rgb(r, g, b)`. */
export function parseRgb(c: string): [number, number, number] | null {
  const s = c.trim();
  const m = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(s);
  if (m) {
    const h = m[1]!.length === 3 ? m[1]!.replace(/./g, (x) => x + x) : m[1]!;
    return [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16)) as [number, number, number];
  }
  const n = /^rgba?\(\s*(\d+)[\s,]+(\d+)[\s,]+(\d+)/i.exec(s);
  return n ? [Number(n[1]), Number(n[2]), Number(n[3])] : null;
}
