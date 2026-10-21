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
    expect(input.destinations.map((d) => d.id)).toEqual([draft.rows[0]!.id, "youtube"]);
    expect(draft.rows[0]!.id).toMatch(/^facebook-[0-9a-f]{8}$/);
    // Removing a destination and adding the same kind again gives a new id, never the old one.
    const again = addRow({ ...draft, rows: draft.rows.slice(1) }, PRESETS[2]);
    expect(again.rows[1]!.id).not.toBe(draft.rows[0]!.id);
    expect(input.destinations[0]!.key).toBe("fb-key");
    expect("key" in input.destinations[1]!).toBe(false);
    expect(input.bandwidth).toEqual({ mode: "auto" });
  });
});
