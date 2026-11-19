// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) The GeekCast Radio Network, LLC.
// This file is part of Altered Audio Editor.

import { describe, expect, it } from "vitest";
import { mixColor, parseRgb } from "./theme";

describe("theme colors", () => {
  it("reads hex and rgb colors", () => {
    expect(parseRgb("#fff")).toEqual([255, 255, 255]);
    expect(parseRgb(" #3d8f6a ")).toEqual([61, 143, 106]);
    expect(parseRgb("rgb(1, 2, 3)")).toEqual([1, 2, 3]);
    expect(parseRgb("rgba(1 2 3 / 0.5)")).toEqual([1, 2, 3]);
    expect(parseRgb("red")).toBeNull();
  });

  it("mixes toward a target color", () => {
    expect(mixColor("#000000", "#ffffff", 0.5)).toBe("rgb(128, 128, 128)");
    expect(mixColor("#3d8f6a", "#ffffff", 0)).toBe("rgb(61, 143, 106)");
    expect(mixColor("#3d8f6a", "#000000", 1)).toBe("rgb(0, 0, 0)");
    expect(mixColor("red", "#000000", 1)).toBe("red");
  });
});
