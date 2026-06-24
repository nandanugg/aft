/// <reference path="../bun-test.d.ts" />

import { describe, expect, test } from "bun:test";
import {
  drainNotifications,
  isTuiConnected,
  pushNotification,
} from "../shared/rpc-notifications.js";

describe("rpc notifications", () => {
  test("keeps messages queued until the client acks their id", () => {
    const initial = drainNotifications(Number.MAX_SAFE_INTEGER);
    expect(initial).toEqual([]);

    pushNotification("one", { ok: true }, "ses_1");
    const firstPoll = drainNotifications();
    expect(firstPoll).toHaveLength(1);
    expect(firstPoll[0]?.type).toBe("one");

    const retryPoll = drainNotifications();
    expect(retryPoll.map((message) => message.id)).toEqual(firstPoll.map((message) => message.id));

    const lastReceivedId = Math.max(...firstPoll.map((message) => message.id));
    expect(drainNotifications(lastReceivedId)).toEqual([]);
  });

  test("scopes drain to the requesting session; other sessions' items survive", () => {
    // Drain everything left from prior tests.
    drainNotifications(Number.MAX_SAFE_INTEGER);

    pushNotification("for-a", { action: "show-status-dialog" }, "ses_A");
    pushNotification("for-b", { action: "show-status-dialog" }, "ses_B");
    pushNotification("global", { action: "show-status-dialog" });

    // Session A sees only its own item + the global one, never ses_B's.
    const aPoll = drainNotifications(0, "ses_A");
    expect(aPoll.map((message) => message.type).sort()).toEqual(["for-a", "global"]);

    // Acking session A must NOT prune session B's still-unseen notification.
    const ackId = Math.max(...aPoll.map((message) => message.id));
    drainNotifications(ackId, "ses_A");
    const bPoll = drainNotifications(0, "ses_B");
    expect(bPoll.map((message) => message.type)).toContain("for-b");
  });

  test("session-less drain still receives all items", () => {
    drainNotifications(Number.MAX_SAFE_INTEGER);
    pushNotification("x", { ok: true }, "ses_1");
    pushNotification("y", { ok: true }, "ses_2");
    const poll = drainNotifications(0);
    expect(poll.map((message) => message.type).sort()).toEqual(["x", "y"]);
  });

  test("isTuiConnected is per-session: a TUI on session A does not mark session B connected", () => {
    // A TUI draining for tuiA must not make tuiB's producers think a TUI is
    // polling for tuiB (which would route tuiB's /aft-status to the dialog path
    // and lose it in the unrelated TUI). Use ids no other test drains so the
    // per-session window is unambiguous.
    drainNotifications(0, "ses_tuiA_only");
    expect(isTuiConnected("ses_tuiA_only")).toBe(true);
    expect(isTuiConnected("ses_tuiB_never_drained")).toBe(false);
    // The session-less (global) query still reports recent activity for legacy
    // callers that have no session context.
    expect(isTuiConnected()).toBe(true);
  });

  test("queue-cap eviction is session-fair: a noisy session cannot evict another session's newest unseen item", () => {
    drainNotifications(Number.MAX_SAFE_INTEGER);
    // One quiet session with a single pending dialog.
    pushNotification("quiet-dialog", { action: "show-status-dialog" }, "ses_quiet");
    // A noisy session floods well past the 100 cap.
    for (let i = 0; i < 200; i += 1) {
      pushNotification("noise", { i }, "ses_noisy");
    }
    // The quiet session's newest item must survive the eviction.
    const quietPoll = drainNotifications(0, "ses_quiet");
    expect(quietPoll.some((message) => message.type === "quiet-dialog")).toBe(true);
  });
});
