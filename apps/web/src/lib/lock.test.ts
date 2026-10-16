import { describe, expect, it } from "vitest";
import { isLocked, lock, unlock } from "./lock";

describe("volunteer mode", () => {
  it("stays locked until the right PIN, and waits after five wrong ones", async () => {
    await expect(lock("12")).rejects.toThrow();
    await lock("2468");
    expect(isLocked()).toBe(true);
    const t = 1_000_000;
    for (let i = 0; i < 5; i++) expect((await unlock("1111", t)).ok).toBe(false);
    // Even the right PIN waits out the pause, then works.
    expect(await unlock("2468", t + 1000)).toEqual({ ok: false, message: "Too many tries. Wait 29 s." });
    expect((await unlock("2468", t + 31_000)).ok).toBe(true);
    expect(isLocked()).toBe(false);
  });
});
