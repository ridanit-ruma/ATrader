import { describe, expect, it } from "vitest";
import { fmtNum, fmtKrw, fmtPct, tone } from "./format";

describe("format", () => {
  it("formats money and percentages", () => {
    expect(fmtKrw(1234567.4)).toBe("1,234,567");
    expect(fmtKrw("-1000.6")).toBe("-1,001");
    expect(fmtPct(-1.234)).toBe("-1.23%");
    expect(fmtPct(2)).toBe("+2.00%");
    expect(fmtNum(0.00012345)).toBe("0.00012345");
    expect(fmtKrw(null)).toBe("—");
  });

  it("colours up red and down blue unless green-up is chosen", () => {
    expect(tone(1, "red-up")).toBe("text-up-red");
    expect(tone(-1, "red-up")).toBe("text-down-blue");
    expect(tone(0, "red-up")).toBe("");
    expect(tone(1, "green-up")).toBe("text-up-green");
    expect(tone(-1, "green-up")).toBe("text-down-red");
  });
});
