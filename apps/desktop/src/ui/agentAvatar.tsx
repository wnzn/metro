// A Space agent's picture, or its initial on its colour, as a rounded square
// (people are round: ./chat.tsx Avatar), and the palette an owner picks that
// colour from. Like ./primitives, feature code passes only semantic props:
// the colour is one of the server's eight palette values (space_agents.rs
// COLORS), mapped here to fills.
//
// A picture's `src` is a `blob:` URL made from bytes fetched with the user's
// credential (src/features/space/agents.ts), never a remote URL.

import { Image, Text, View } from "nsh";
import { Button } from "nsh/base";
import { cx, focusRingClass, transitionClass } from "./tokens";

type Testable = { testID?: string; accessibilityLabel?: string };

export type AgentAvatarSize = "xs" | "sm" | "md" | "lg";

/** The server's palette, in its order, with each colour's fill and a name for screen readers. */
export const AGENT_PALETTE: readonly { value: string; name: string; fill: string }[] = [
  { value: "#4f46e5", name: "Indigo", fill: "bg-[#4f46e5]" },
  { value: "#0891b2", name: "Cyan", fill: "bg-[#0891b2]" },
  { value: "#059669", name: "Green", fill: "bg-[#059669]" },
  { value: "#ca8a04", name: "Amber", fill: "bg-[#ca8a04]" },
  { value: "#ea580c", name: "Orange", fill: "bg-[#ea580c]" },
  { value: "#dc2626", name: "Red", fill: "bg-[#dc2626]" },
  { value: "#db2777", name: "Pink", fill: "bg-[#db2777]" },
  { value: "#7c3aed", name: "Violet", fill: "bg-[#7c3aed]" },
];

const fillOf = (color: string) => (AGENT_PALETTE.find((c) => c.value === color.toLowerCase()) ?? AGENT_PALETTE[0]!).fill;

const box: Record<AgentAvatarSize, string> = {
  xs: "size-[16px] rounded-[4px]",
  sm: "size-[20px] rounded-[5px]",
  md: "size-[32px] rounded-(--radius-sm)",
  lg: "size-[72px] rounded-(--radius-lg)",
};
const ink: Record<AgentAvatarSize, string> = {
  xs: "text-[9px]",
  sm: "text-[10px]",
  md: "text-(length:--text-sm)",
  lg: "text-(length:--text-2xl)",
};

/**
 * An agent's picture, or its `initial` on its `color` when there is none (or
 * it failed to draw). Decorative unless it has an `accessibilityLabel`. The
 * picture's testID is `${testID}:image`.
 */
export function AgentAvatar(
  props: Testable & {
    initial: string;
    color: string;
    src?: string | null;
    size?: AgentAvatarSize;
    onImageError?: () => void;
  },
) {
  const size = () => props.size ?? "md";
  return (
    <View
      class={cx("shrink-0 items-center justify-center overflow-hidden", box[size()], fillOf(props.color))}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel}
      accessibilityElementsHidden={props.accessibilityLabel ? undefined : true}
      role={props.accessibilityLabel ? "image" : undefined}
    >
      <If when={props.src} else={<Text class={cx("font-semibold text-white", ink[size()])}>{props.initial}</Text>}>
        {(src) => (
          <Image
            src={src()}
            class="w-full h-full"
            style={{ objectFit: "cover" }}
            testID={props.testID ? `${props.testID}:image` : undefined}
            onError={() => props.onImageError?.()}
          />
        )}
      </If>
    </View>
  );
}

/**
 * The eight agent colours as swatches; the chosen one is ringed. Each
 * swatch's testID is `${testID}:${hex}`; screen readers hear its name.
 */
export function ColorSwatches(props: Testable & { value: string; onChange: (color: string) => void; disabled?: boolean }) {
  return (
    <View class="flex-row flex-wrap gap-(--space-2)" testID={props.testID} role="radiogroup" accessibilityLabel={props.accessibilityLabel ?? "Colour"}>
      <Each of={AGENT_PALETTE} key={(c) => c.value}>
        {(c) => (
          <Button
            class={cx(
              "size-[28px] rounded-(--radius-sm) border-2 disabled:opacity-50",
              c().fill,
              props.value.toLowerCase() === c().value ? "border-foreground" : "border-transparent hover:border-border",
              focusRingClass,
              transitionClass,
            )}
            role="radio"
            selected={props.value.toLowerCase() === c().value}
            disabled={props.disabled}
            onPress={() => props.onChange(c().value)}
            accessibilityLabel={c().name}
            testID={props.testID ? `${props.testID}:${c().value}` : undefined}
          />
        )}
      </Each>
    </View>
  );
}
