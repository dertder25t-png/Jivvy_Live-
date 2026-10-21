import { describe, expect, it } from "vitest";
import { PRESETS, addRow, fromView, moveRow, toInput } from "./streamSettings";

describe("streaming settings draft", () => {
  it("never sends a key it wasn't given, and keeps the order the tech lead chose", () => {
    let draft = fromView({
      destinations: [{ id: "youtube", name: "YouTube", url: "rtmps://a.rtmps.youtube.com:443/live2", enabled: true, hasKey: true }],
    });
    expect(draft.rows[0]).toMatchObject({ key: "", hasKey: true });
    draft = addRow(draft, PRESETS[2]); // Facebook
    draft.rows[1]!.key = " fb-key ";
    draft = moveRow(draft, 1, -1);
    const input = toInput(draft);
    expect(input.destinations.map((d) => d.id)).toEqual(["facebook", "youtube"]);
    expect(input.destinations[0]!.key).toBe("fb-key");
    expect("key" in input.destinations[1]!).toBe(false);
    expect(input.bandwidth).toEqual({ mode: "auto" });
  });
});
