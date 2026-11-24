// A side panel's resize handle: a thin band on the panel's inner edge.
//
// - Pointer: drag it (PanResponder; the width follows each move, and is
//   saved when the drag ends). The resize cursor while over it, a line in
//   the ring colour while hovered, dragged or focused from the keyboard. A
//   double click puts the default width back. No tooltip: on a band as tall
//   as the panel it would cover the header's controls; the cursor says it.
// - Keyboard: it is a stop of the focus (a press focuses it too); Left and
//   Right move the edge by 16 points (Shift: 64), Home and End go to the
//   narrowest and widest, Return or Space (its activation) restores the
//   default.
// - Screen readers: an adjustable element named for what it resizes, its
//   value the width in points, increment and decrement as the arrows.
//
// `collapsible`: a drag that ends well below the narrowest width collapses
// the panel instead (the navigation snaps to its icon rail), keeping the
// width it had for when it opens again.

import { effect, keymap, PanResponder, signal, untracked, View } from "nsh";
import type { ViewRef } from "nsh";
import type { LayoutPanel } from "./panels";
import { cx, transitionClass } from "./tokens";

const STEP = 16;
const BIG_STEP = 64;
/** How far past the narrowest width a drag must end to collapse a collapsible panel. */
const SNAP = 48;
/** Two releases without a move this close together are a double click. */
const DOUBLE_MS = 400;

export function ResizeHandle(props: {
  panel: LayoutPanel;
  /** Which edge of the panel the handle is on: `end` for a panel on the left (the navigation), `start` for one on the right (the runs). */
  edge: "start" | "end";
  /** What it resizes, as screen readers say it ("workspace navigation"). */
  name: string;
  collapsible?: boolean;
  testID?: string;
}) {
  const p = props.panel;
  const dir = props.edge === "end" ? 1 : -1;
  const dragging = signal(false);
  let node: ViewRef | undefined;
  const focused = signal(false);
  let start = 0;
  let lastTap = 0;
  /** A pointer went down on the handle and its press (if any) is still to come. */
  let pointerPress = false;
  const finish = (dx: number) => {
    dragging.set(false);
    const raw = start + dir * dx;
    if (Math.abs(dx) < 3) {
      // A click: two of them close together restore the default width.
      const now = Date.now();
      if (now - lastTap < DOUBLE_MS) {
        lastTap = 0;
        p.resetWidth();
      } else lastTap = now;
      return;
    }
    lastTap = 0;
    pointerPress = false; // a drag: no press follows
    if (props.collapsible && raw < p.size.min - SNAP) {
      p.setWidth(start);
      p.collapse();
      return;
    }
    p.setWidth(raw);
  };
  const pan = PanResponder.create({
    onStartShouldSetPanResponder: () => true,
    onMoveShouldSetPanResponder: () => true,
    onPanResponderTerminationRequest: () => false,
    onPanResponderGrant: () => {
      // A press takes the focus, as Tab does, so the arrows work next.
      node?.focus();
      pointerPress = true;
      start = untracked(p.width);
      dragging.set(true);
    },
    onPanResponderMove: (_e, g) => p.setWidth(start + dir * g.dx, false),
    onPanResponderRelease: (_e, g) => finish(g.dx),
    onPanResponderTerminate: (_e, g) => finish(g.dx),
  });
  const nudge = (by: number) => p.setWidth(untracked(p.width) + dir * by);
  effect(() => {
    if (!focused()) return;
    return keymap.bind(
      {
        left: () => nudge(-STEP),
        right: () => nudge(STEP),
        "shift+left": () => nudge(-BIG_STEP),
        "shift+right": () => nudge(BIG_STEP),
        home: () => p.setWidth(p.size.min),
        end: () => p.setWidth(p.size.max),
      },
      { when: "always", repeat: true },
    );
  });
  const label = () => `Resize ${props.name}`;
  return (
    <View
      {...pan.panHandlers}
      ref={(n) => (node = n)}
      class={cx(
        "group self-stretch w-[9px] shrink-0 items-center justify-center",
        // Over the panel's edge, half on each side, above both.
        "-mx-[4px] z-10",
      )}
      style={{ cursor: "col-resize" }}
      focusable
      role="adjustable"
      testID={props.testID}
      accessibilityLabel={label()}
      accessibilityHint="Left and Right resize it; Return restores the default width"
      accessibilityValue={{ min: p.size.min, max: p.size.max, now: p.width(), text: `${p.width()} points wide` }}
      accessibilityActions={[{ name: "increment" }, { name: "decrement" }]}
      onAccessibilityAction={(e) => {
        if (e.actionName === "increment") nudge(STEP * dir);
        else if (e.actionName === "decrement") nudge(-STEP * dir);
      }}
      onPress={(e) => {
        // A pointer's press comes after its grant; one without (Return or Space on the focused handle) is the keyboard's.
        const keyboard = !pointerPress;
        pointerPress = false;
        if (keyboard || e?.clickCount === 2) p.resetWidth();
      }}
      onFocus={() => focused.set(true)}
      onBlur={() => focused.set(false)}
    >
      <View
        class={cx(
          "h-full w-[2px] rounded-full group-hover:bg-ring group-focus-visible:bg-ring",
          transitionClass,
          // Lit while dragged; focus shows only when the keyboard moved it there (focus-visible).
          dragging() ? "bg-ring" : "bg-transparent",
        )}
        accessibilityElementsHidden
      />
    </View>
  );
}
