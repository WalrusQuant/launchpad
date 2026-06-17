import { describe, it, expect } from "vitest";
import { formatBlameAge } from "./blamegutter.js";

describe("formatBlameAge", () => {
  const now = () => Math.floor(Date.now() / 1000);

  it("renders sub-minute ages as 'now'", () => {
    expect(formatBlameAge(now())).toBe("now");
    expect(formatBlameAge(now() - 30)).toBe("now");
  });

  it("renders minutes / hours / days / weeks", () => {
    expect(formatBlameAge(now() - 5 * 60)).toBe("5m");
    expect(formatBlameAge(now() - 3 * 3600)).toBe("3h");
    expect(formatBlameAge(now() - 2 * 86400)).toBe("2d");
    expect(formatBlameAge(now() - 2 * 604800)).toBe("2w");
  });

  it("renders months and years", () => {
    expect(formatBlameAge(now() - 3 * 2592000)).toBe("3mo");
    expect(formatBlameAge(now() - 2 * 31536000)).toBe("2y");
  });
});
