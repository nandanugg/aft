import { describe, expect, test } from "bun:test";

import { badgeTextColor, readableTextColorOn } from "../tui/badge-contrast";

type Color = { r: number; g: number; b: number; a?: number };

function color(overrides: Partial<Color>): Color {
  return { r: 0, g: 0, b: 0, a: 1, ...overrides };
}

describe("badgeTextColor", () => {
  test("returns the theme background when it is opaque and distinct from the accent", () => {
    const accent = color({ r: 0.82, g: 0.3, b: 0.25 });
    const background = color({ r: 0.06, g: 0.08, b: 0.1, a: 1 });

    expect(badgeTextColor(accent, background)).toBe(background);
  });

  test("falls back to a luminance pick when the theme background is transparent", () => {
    const accent = color({ r: 0.12, g: 0.18, b: 0.24 });
    const background = color({ r: 0.01, g: 0.01, b: 0.01, a: 0 });

    expect(badgeTextColor(accent, background)).toBe("#ffffff");
  });

  test("falls back to a luminance pick when the theme background is nearly the accent", () => {
    const accent = color({ r: 0.9, g: 0.88, b: 0.84 });
    const background = color({ r: 0.93, g: 0.9, b: 0.87, a: 1 });

    expect(badgeTextColor(accent, background)).toBe("#000000");
  });
});

describe("readableTextColorOn", () => {
  test("returns white for dark accents", () => {
    expect(readableTextColorOn(color({ r: 0.12, g: 0.18, b: 0.24 }))).toBe("#ffffff");
  });

  test("returns black for light accents", () => {
    expect(readableTextColorOn(color({ r: 0.94, g: 0.92, b: 0.88 }))).toBe("#000000");
  });
});
