// App chrome primitives: icons, the modal frame, brand marks, the provider
// picker and the search box. Like ./primitives, feature code passes only
// semantic props; every class and color decision lives here or in
// theme/theme.json.
//
// Icons are one family drawn for Metro (./icons, 24-unit grid, 1.8 stroke,
// round caps), monochrome templates tinted with theme tokens, so they follow
// light and dark. Provider marks are bundled monochrome SVGs from
// @lobehub/icons-static-svg 1.105.0 (MIT; ./icons/LICENSE-brand-marks.txt),
// tinted with the theme's foreground, shown only beside the provider's own
// name; any other provider gets a lettermark. Nothing is fetched at runtime.

import { Image, Overlay, Scroll, Text, View, cleanup, effect, keymap, reduceMotion, signal, token, untracked, windowSize } from "nsh";
import type { ImageAsset, InputRef, JSX, ViewRef } from "nsh";
import { Button, Dialog, Input, Menu, MenuItem, MenuSeparator, Select } from "nsh/base";
import agentIcon from "./icons/agent.svg";
import appearanceIcon from "./icons/appearance.svg";
import chatIcon from "./icons/chat.svg";
import fileIcon from "./icons/file.svg";
import libraryIcon from "./icons/library.svg";
import metroIcon from "./icons/metro.svg";
import moreIcon from "./icons/more.svg";
import sendIcon from "./icons/send.svg";
import checkIcon from "./icons/check.svg";
import chevronDownIcon from "./icons/chevron-down.svg";
import chevronRightIcon from "./icons/chevron-right.svg";
import clockIcon from "./icons/clock.svg";
import closeIcon from "./icons/close.svg";
import copyIcon from "./icons/copy.svg";
import deviceIcon from "./icons/device.svg";
import hashIcon from "./icons/hash.svg";
import logoutIcon from "./icons/logout.svg";
import personIcon from "./icons/person.svg";
import plusIcon from "./icons/plus.svg";
import providersIcon from "./icons/providers.svg";
import routingIcon from "./icons/routing.svg";
import runnerIcon from "./icons/runner.svg";
import searchIcon from "./icons/search.svg";
import serverIcon from "./icons/server.svg";
import settingsIcon from "./icons/settings.svg";
import threadIcon from "./icons/thread.svg";
import brandOpenai from "./icons/brand-openai.svg";
import brandAnthropic from "./icons/brand-anthropic.svg";
import brandGemini from "./icons/brand-gemini.svg";
import brandEmbeddinggemma from "./icons/brand-embeddinggemma.svg";
import brandAzureOpenai from "./icons/brand-azure-openai.svg";
import brandDeepseek from "./icons/brand-deepseek.svg";
import brandGroq from "./icons/brand-groq.svg";
import brandTogether from "./icons/brand-together.svg";
import brandHuggingface from "./icons/brand-huggingface.svg";
import brandMistral from "./icons/brand-mistral.svg";
import brandOpenrouter from "./icons/brand-openrouter.svg";
import brandVenice from "./icons/brand-venice.svg";
import brandZai from "./icons/brand-zai.svg";
import brandMinimax from "./icons/brand-minimax.svg";
import brandMoonshot from "./icons/brand-moonshot.svg";
import brandXiaomimimo from "./icons/brand-xiaomimimo.svg";
import brandCohere from "./icons/brand-cohere.svg";
import brandOllama from "./icons/brand-ollama.svg";
import brandOpencode from "./icons/brand-opencode.svg";
import brandXai from "./icons/brand-xai.svg";
import brandSystemone from "./icons/brand-systemone.svg";
import {
  controlEdgeClass,
  controlHeightClass,
  cx,
  disabledClass,
  focusRingClass,
  popoverClass,
  popoverItemClass,
  popoverMotion,
  textClass,
  toneSurfaceClass,
  toneTextClass,
  tooltipClass,
  transitionClass,
} from "./tokens";
import type { Tone } from "./tokens";

type Testable = { testID?: string; accessibilityLabel?: string };

// ---------------------------------------------------------------- icons

// The one map of the app's icon names. Any feature may use any name here;
// a glyph only one screen draws can stay a local import beside it.
const ICONS = {
  agent: agentIcon,
  appearance: appearanceIcon,
  chat: chatIcon,
  check: checkIcon,
  "chevron-down": chevronDownIcon,
  "chevron-right": chevronRightIcon,
  clock: clockIcon,
  close: closeIcon,
  copy: copyIcon,
  device: deviceIcon,
  file: fileIcon,
  hash: hashIcon,
  library: libraryIcon,
  logout: logoutIcon,
  metro: metroIcon,
  more: moreIcon,
  person: personIcon,
  plus: plusIcon,
  providers: providersIcon,
  routing: routingIcon,
  runner: runnerIcon,
  search: searchIcon,
  send: sendIcon,
  server: serverIcon,
  settings: settingsIcon,
  thread: threadIcon,
} as const;

export type IconName = keyof typeof ICONS;
export type IconColor = "default" | "muted" | "sidebar" | "sidebar-active" | "accent" | "danger" | "on-primary";

const ICON_TOKEN = {
  default: "foreground",
  muted: "muted-foreground",
  sidebar: "sidebar-foreground",
  "sidebar-active": "sidebar-accent-foreground",
  accent: "accent-foreground",
  danger: "destructive",
  "on-primary": "primary-foreground",
} as const;

/** A decorative icon: screen readers skip it; say what it means in the control's label. */
export function Icon(props: { name: IconName; color?: IconColor; size?: 12 | 14 | 16 | 20 }) {
  const size = () => props.size ?? 16;
  return (
    <Image
      src={ICONS[props.name]}
      fadeDuration={0}
      style={{ width: size(), height: size(), tintColor: token(ICON_TOKEN[props.color ?? "muted"]) }}
      accessibilityElementsHidden
    />
  );
}

const focusRing = focusRingClass;

/** A square button with only an icon. `accessibilityLabel` is required: it is all a screen reader hears. */
export function IconButton(props: { icon: IconName; accessibilityLabel: string; onPress: () => void; testID?: string; disabled?: boolean; tooltip?: string }) {
  return (
    <Button
      class={cx("items-center justify-center size-[30px] rounded-(--radius-sm) hover:bg-muted active:bg-muted", transitionClass, disabledClass, focusRing)}
      onPress={props.onPress}
      disabled={props.disabled}
      accessibilityLabel={props.accessibilityLabel}
      testID={props.testID}
      tooltip={props.tooltip ? <Text class={tooltipClass}>{props.tooltip}</Text> : undefined}
    >
      <Icon name={props.icon} color="muted" />
    </Button>
  );
}

// ---------------------------------------------------------------- brand

/** Metro's own mark: an M on the primary colour. Decorative; the name beside it is what screen readers hear. */
export function MetroMark(props: { size?: "sm" | "lg" }) {
  const lg = () => props.size === "lg";
  return (
    <View
      class={cx("items-center justify-center bg-primary", lg() ? "size-[36px] rounded-(--radius)" : "size-[22px] rounded-[6px]")}
      accessibilityElementsHidden
      testID="metro-mark"
    >
      <Image src={metroIcon} fadeDuration={0} style={{ width: lg() ? 22 : 15, height: lg() ? 22 : 15, tintColor: token("primary-foreground") }} accessibilityElementsHidden />
    </View>
  );
}

// ---------------------------------------------------------------- account menu

/** One account action: `ActionsMenuItem`'s shape (./primitives), plus an icon. */
export type AccountMenuItem = {
  /** Its testID. */
  id: string;
  label: string;
  onPress: () => void;
  icon?: IconName;
  /** Ends something (Sign out): the danger colour, last, after a line. */
  destructive?: boolean;
  disabled?: boolean;
};

/**
 * Who is signed in, where: a picture, a name and one quiet line (the
 * server), pressed to open a menu of account actions. The trigger carries
 * `testID`; each item its own `id`. `note` is a short line above the items
 * (the user's ID), not an action.
 */
export function AccountMenu(props: {
  testID?: string;
  accessibilityLabel: string;
  avatar: JSX.Element;
  name: string;
  detail: string;
  detailIcon?: IconName;
  note?: string;
  items: readonly AccountMenuItem[];
}) {
  const plain = () => props.items.filter((i) => !i.destructive);
  const ending = () => props.items.filter((i) => i.destructive);
  const still = () => reduceMotion();
  return (
    <Menu
      placement="top"
      align="start"
      offset={6}
      popupClass={cx(popoverClass, "min-w-[232px]")}
      popupStartingClass={still() ? undefined : popoverMotion.starting}
      popupEndingClass={still() ? undefined : popoverMotion.ending}
      backdropClass="bg-black/40"
      trigger={
        <View
          class={cx("flex-row items-center gap-(--space-2) rounded-(--radius-sm) px-(--space-2) py-(--space-2) hover:bg-sidebar-hover active:bg-sidebar-hover", transitionClass, focusRing)}
          focusable
          testID={props.testID}
          accessibilityLabel={props.accessibilityLabel}
        >
          {props.avatar}
          <View class="flex-1 min-w-0 flex-col">
            <Text class={cx(textClass.sm, "font-semibold text-sidebar-foreground")} style={{ numberOfLines: 1 }}>
              {props.name}
            </Text>
            <View class="flex-row items-center gap-[4px] min-w-0">
              <If when={props.detailIcon}>{(name) => <Icon name={name()} color="muted" size={12} />}</If>
              <Text class={cx("flex-1", textClass.xs, "text-muted-foreground")} style={{ numberOfLines: 1 }}>
                {props.detail}
              </Text>
            </View>
          </View>
          <Icon name="more" color="muted" />
        </View>
      }
    >
      <If when={props.note}>
        <Text class={cx("px-(--space-2) pt-[4px] pb-[6px] font-mono text-muted-foreground", textClass.xs)} style={{ numberOfLines: 1 }}>
          {props.note}
        </Text>
      </If>
      <Each of={plain()} key={(i) => i.id}>
        {(item) => <AccountEntry item={item()} />}
      </Each>
      <If when={plain().length > 0 && ending().length > 0}>
        <MenuSeparator class="my-[4px] h-px bg-border" />
      </If>
      <Each of={ending()} key={(i) => i.id}>
        {(item) => <AccountEntry item={item()} />}
      </Each>
    </Menu>
  );
}

function AccountEntry(props: { item: AccountMenuItem }) {
  return (
    <MenuItem
      class={cx("flex-row items-center gap-(--space-2)", popoverItemClass)}
      testID={props.item.id}
      disabled={props.item.disabled}
      accessibilityLabel={props.item.label}
      onSelect={() => props.item.onPress()}
    >
      <If when={props.item.icon}>{(name) => <Icon name={name()} color={props.item.destructive ? "danger" : "muted"} />}</If>
      <Text class={cx(textClass.sm, props.item.destructive ? "text-tone-danger-foreground" : "text-popover-foreground")}>{props.item.label}</Text>
    </MenuItem>
  );
}

// ---------------------------------------------------------------- responsive

/** The window is narrow: panels stack, side navigation turns into tabs. */
export const compactWidth = () => windowSize().width < 900;

// ---------------------------------------------------------------- modal

/** How many `Modal`s are open: the app's shortcuts leave the page under one alone. */
const openModals = signal(0);
/** A `Modal` is open over the app. */
export const modalOpen = () => openModals() > 0;

/**
 * A large modal over the app, in its own native layer: it takes the keyboard
 * focus and gives it back as it closes; Escape, the backdrop and `onClose`'s
 * callers close it. Sized to the window, never past it: a compact window
 * gets a narrower margin. With reduced motion it appears without a fade.
 */
export function Modal(props: Testable & { open: boolean; title: string; onClose: () => void; children?: JSX.Element }) {
  effect(() => {
    if (!props.open) return;
    openModals.set(untracked(openModals) + 1);
    return () => openModals.set(untracked(openModals) - 1);
  });
  const size = () => {
    const w = windowSize();
    const margin = compactWidth() ? 16 : 48;
    return { width: Math.max(320, Math.min(1080, w.width - margin)), height: Math.max(360, Math.min(800, w.height - margin)) };
  };
  return (
    <Dialog
      open={props.open}
      animation={reduceMotion() ? "none" : "fade"}
      accessibilityLabel={props.title}
      testID={props.testID}
      backdropTestID={props.testID ? `${props.testID}-backdrop` : undefined}
      onOpenChange={(e) => {
        if (!e.open) props.onClose();
      }}
      backdropClass="bg-black/55"
      class="m-auto flex-col overflow-hidden rounded-(--radius-lg) border border-border bg-background shadow-lg"
      style={size()}
    >
      {props.children}
    </Dialog>
  );
}

/** The side navigation of a modal (Settings' sections). */
export function ModalNav(props: Testable & { children?: JSX.Element }) {
  return (
    <View class="w-[228px] flex-col gap-[2px] bg-sidebar border-r border-sidebar-border px-(--space-2) py-(--space-3)" testID={props.testID} accessibilityLabel={props.accessibilityLabel} role="tablist">
      {props.children}
    </View>
  );
}

/** One section of a modal's navigation: a tab, side by side or (compact) in a row. */
export function ModalTab(props: { label: string; icon: IconName; selected: boolean; onPress: () => void; testID?: string; compact?: boolean }) {
  return (
    <Button
      class={cx(
        "flex-row items-center gap-(--space-2) rounded-(--radius-sm) px-(--space-2) py-[7px]",
        transitionClass,
        focusRing,
        // 40px rows in the side nav; the compact row of tabs keeps its own height.
        props.compact ? "px-(--space-3)" : "min-h-[40px]",
        props.selected ? "bg-sidebar-accent" : "hover:bg-sidebar-hover active:bg-sidebar-hover",
      )}
      role="tab"
      selected={props.selected}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.label}
    >
      <Icon name={props.icon} color={props.selected ? "sidebar-active" : "sidebar"} />
      <Text class={cx("text-(length:--text-sm)", props.selected ? "font-semibold text-sidebar-accent-foreground" : "text-sidebar-foreground")} style={{ numberOfLines: 1 }}>
        {props.label}
      </Text>
    </Button>
  );
}

/** A modal's tabs in one wrapping row (compact windows). */
export function ModalTabRow(props: Testable & { children?: JSX.Element }) {
  return (
    <View class="flex-row flex-wrap gap-[2px] bg-sidebar border-b border-sidebar-border px-(--space-2) py-(--space-2)" testID={props.testID} role="tablist">
      {props.children}
    </View>
  );
}

// ---------------------------------------------------------------- brand marks

/** Bundled provider marks, by Metro's provider key (see ./icons/LICENSE-brand-marks.txt). */
const BRAND_LOGOS: Record<string, ImageAsset> = {
  "openai": brandOpenai,
  "anthropic": brandAnthropic,
  "gemini": brandGemini,
  "embeddinggemma": brandEmbeddinggemma,
  "azure-openai": brandAzureOpenai,
  "deepseek": brandDeepseek,
  "groq": brandGroq,
  "together": brandTogether,
  "huggingface": brandHuggingface,
  "mistral": brandMistral,
  "openrouter": brandOpenrouter,
  "venice": brandVenice,
  "zai": brandZai,
  "minimax": brandMinimax,
  "moonshot": brandMoonshot,
  "xiaomimimo": brandXiaomimimo,
  "cohere": brandCohere,
  "ollama": brandOllama,
  "opencode": brandOpencode,
  "xai": brandXai,
  "systemone": brandSystemone,
};

/** Whether a bundled mark exists for this provider key. */
export const hasBrandLogo = (key: string) => Object.prototype.hasOwnProperty.call(BRAND_LOGOS, key);

/**
 * How a provider is drawn: its bundled mark (`logo`, a key of the bundled
 * marks) when there is one, else a short lettermark in one of the theme's tones.
 */
export type Mark = { letters: string; tone: Tone; logo?: string };

/** A provider's mark tile. Decorative: the name next to it is what screen readers hear. */
export function BrandMark(props: { mark: Mark; size?: "sm" | "md" }) {
  const logo = () => (props.mark.logo && hasBrandLogo(props.mark.logo) ? props.mark.logo : null);
  const md = () => props.size === "md";
  return (
    <If
      when={logo()}
      else={
        <View
          class={cx("items-center justify-center rounded-(--radius-sm)", md() ? "size-[30px]" : "size-[22px]", toneSurfaceClass[props.mark.tone])}
          accessibilityElementsHidden
          testID={`mark-letters:${props.mark.letters}`}
        >
          <Text class={cx("font-semibold", md() ? "text-(length:--text-sm)" : "text-[10px]", toneTextClass[props.mark.tone])}>{props.mark.letters}</Text>
        </View>
      }
    >
      {(key) => (
        <View
          class={cx("items-center justify-center rounded-(--radius-sm) border border-border bg-background", md() ? "size-[30px]" : "size-[22px]")}
          accessibilityElementsHidden
          testID={`mark-logo:${key()}`}
        >
          <Image
            src={BRAND_LOGOS[key()]}
            fadeDuration={0}
            style={{ width: md() ? 18 : 14, height: md() ? 18 : 14, tintColor: token("foreground") }}
            accessibilityElementsHidden
          />
        </View>
      )}
    </If>
  );
}

export type BrandItem = { value: string; label: string; hint?: string; mark: Mark; disabled?: boolean };

/**
 * A compact drop-down of brands (model providers): each row a lettermark, a
 * name and a short hint. Searchable; the trigger shows the chosen one.
 */
export function BrandPicker(props: {
  accessibilityLabel: string;
  items: readonly BrandItem[];
  value: string | null;
  onChange: (value: string | null) => void;
  placeholder: string;
  testID?: string;
}) {
  const current = () => props.items.find((item) => item.value === props.value) ?? null;
  const byValue = () => new Map(props.items.map((item) => [item.value, item]));
  return (
    <Select
      class={cx(
        "flex-row items-center gap-(--space-2) min-h-[40px] w-full max-w-[420px] rounded-(--radius-sm) border border-input bg-background px-(--space-2) hover:bg-muted",
        focusRing,
      )}
      items={props.items.map((item) => ({ value: item.value, title: item.hint ? `${item.label} — ${item.hint}` : item.label, disabled: item.disabled }))}
      value={props.value}
      onChange={({ value }) => props.onChange(value)}
      placeholder={props.placeholder}
      searchable
      searchPlaceholder="Search providers"
      searchAccessibilityLabel="Search providers"
      searchTestID={props.testID ? `${props.testID}-search` : undefined}
      accessibilityLabel={props.accessibilityLabel}
      popupClass={popoverClass}
      popupStartingClass={reduceMotion() ? undefined : popoverMotion.starting}
      popupEndingClass={reduceMotion() ? undefined : popoverMotion.ending}
      listClass="max-h-[340px]"
      itemClass={cx("group", popoverItemClass)}
      searchClass="mb-[4px] rounded-(--radius-sm) border border-input bg-background px-(--space-2) py-(--space-1) text-(length:--text-sm) text-foreground"
      empty={<Text class="px-(--space-2) py-(--space-2) text-(length:--text-sm) text-muted-foreground">No provider matches.</Text>}
      testID={props.testID}
      renderItem={(entry) => {
        const item = byValue().get(entry.value);
        return (
          <View class="flex-row items-center gap-(--space-2)" testID={props.testID ? `${props.testID}:${entry.value}` : undefined}>
            <If when={item}>{(i) => <BrandMark mark={i().mark} />}</If>
            <View class="flex-1 min-w-0 flex-col">
              <Text class="text-(length:--text-sm) font-medium text-popover-foreground" style={{ numberOfLines: 1 }}>
                {item?.label ?? entry.title}
              </Text>
              <If when={item?.hint}>
                <Text class="text-(length:--text-xs) text-muted-foreground" style={{ numberOfLines: 1 }}>
                  {item?.hint}
                </Text>
              </If>
            </View>
          </View>
        );
      }}
      trigger={
        <View class="flex-1 flex-row items-center gap-(--space-2)">
          <If when={current()} else={<Icon name="providers" color="muted" />}>
            {(c) => <BrandMark mark={c().mark} />}
          </If>
          <Text class={cx("flex-1 text-(length:--text-sm)", current() ? "font-medium text-foreground" : "text-muted-foreground")} style={{ numberOfLines: 1 }}>
            {current()?.label ?? props.placeholder}
          </Text>
          <Icon name="chevron-down" color="muted" size={14} />
        </View>
      }
    />
  );
}

// ---------------------------------------------------------------- search box

/** The list under the search box: shown while `open`, anchored to the box, as wide as it. */
export type SearchPopup = {
  open: boolean;
  /** Escape, or a press outside the box and the list. */
  onClose: () => void;
  /** Up and Down while the field has the focus and the list is open. */
  onMove: (step: 1 | -1) => void;
  /** What screen readers call the list. */
  accessibilityLabel: string;
  testID?: string;
  children?: JSX.Element;
};

/**
 * The app's search box: a raised pill with a magnifier, the field, an
 * optional trailing control (a mode) and its keyboard shortcut. Return
 * submits. `shortcut` (default `mod+k`, `null` for none) only moves the
 * keyboard focus into the field, as a click does: it changes nothing else,
 * and does nothing while a `Modal` is open over the box. The binding is
 * removed with the box, so signing out and in again never stacks it.
 *
 * `popup` is a list under the box (quick results): anchored to it, as wide
 * as it, never taller than the room below (it scrolls). Its rows never take
 * the keyboard focus, so the caret stays in the field: Up and Down move the
 * highlight (bound only while the list is open and the field focused), Tab
 * leaves as usual.
 */
export function SearchBox(
  props: Testable & {
    value: string;
    onChange: (value: string) => void;
    onSubmit: () => void;
    onFocus?: () => void;
    onBlur?: () => void;
    placeholder: string;
    maxLength?: number;
    trailing?: JSX.Element;
    shortcut?: string | null;
    popup?: SearchPopup;
  },
) {
  let field: InputRef | undefined;
  const box = signal<ViewRef | null>(null);
  const width = signal(0);
  const room = signal(360);
  const focused = signal(false);
  const shortcut = props.shortcut === undefined ? "mod+k" : props.shortcut;
  let hint = "";
  if (shortcut) {
    try {
      const unbind = keymap.bind(
        shortcut,
        () => {
          if (!modalOpen()) field?.focus();
        },
      );
      cleanup(unbind);
      hint = keymap.label(shortcut);
    } catch {
      /* a platform that keeps this keystroke: the box is still a click away */
    }
  }
  effect(() => {
    const popup = props.popup;
    if (!popup?.open || !focused()) return;
    return keymap.bind({ down: () => popup.onMove(1), up: () => popup.onMove(-1) }, { when: "always", repeat: true });
  });
  return (
    <View
      ref={(node) => box.set(node)}
      onLayout={(e) => width.set(e.bounds.size.width)}
      class={cx(
        "flex-row items-center gap-(--space-2) w-full max-w-[560px] min-w-0 rounded-full border bg-popover pl-(--space-3) pr-[6px] focus-within:border-ring focus-within:bg-background",
        controlEdgeClass,
        controlHeightClass.default,
        transitionClass,
      )}
    >
      <Icon name="search" color="muted" />
      <Input
        ref={(node) => (field = node)}
        class={cx("flex-1 min-w-0 py-[6px] text-foreground placeholder:text-muted-foreground", textClass.sm)}
        value={props.value}
        onChange={({ value }) => props.onChange(value)}
        onSubmit={() => props.onSubmit()}
        onFocus={() => {
          focused.set(true);
          props.onFocus?.();
        }}
        onBlur={() => {
          focused.set(false);
          props.onBlur?.();
        }}
        submitBehavior="submit"
        placeholder={props.placeholder}
        maxLength={props.maxLength}
        testID={props.testID}
        accessibilityLabel={props.accessibilityLabel ?? props.placeholder}
      />
      {props.trailing}
      <If when={hint}>
        <View class="rounded-[5px] border border-border bg-muted px-[6px] py-px" accessibilityElementsHidden testID={props.testID ? `${props.testID}-shortcut` : undefined}>
          <Text class={cx(textClass.xs, "font-medium text-muted-foreground")}>{hint}</Text>
        </View>
      </If>
      <If when={props.popup?.open ? props.popup : null}>
        {(popup) => (
          <Overlay
            anchor={box()}
            placement="bottom"
            align="start"
            offset={8}
            margin={8}
            onPressOutside={() => popup().onClose()}
            onCancel={() => popup().onClose()}
            onPosition={(e) => room.set(e.available.height)}
          >
            <View
              class={searchPanel}
              style={{ width: Math.max(300, width()) }}
              role="listbox"
              testID={popup().testID}
              accessibilityLabel={popup().accessibilityLabel}
            >
              <Scroll style={{ maxHeight: Math.max(140, Math.min(440, room() - 16)) }}>
                <View class="flex-col px-[6px]">{popup().children}</View>
              </Scroll>
            </View>
          </Overlay>
        )}
      </If>
    </View>
  );
}

// Quick results are compact rows, one line of identity each, divided by a
// hairline: a leading column 72 wide (a file's tile, a message's glyph), the
// name in the middle as the strongest text with at most one quiet line under
// it, and on the right a file's source (only when it tells two apart) and its
// extension as a small colour-coded badge. The highlighted row (Up and Down)
// has the accent surface and a primary bar in its leading gutter, in both
// themes. Rows never take the keyboard focus: the field keeps it.
//
// Gutters: the panel's list is inset 6 from its edges and every row, heading,
// status line and footer pads 8 more on the left, so tiles, glyphs, headings
// and the footer's icon all start 14 from the panel's edge. On the right the
// badge column ends 16 from it, clear of the scroll bar. Dividers run the
// list's width, inset 6 on both sides.

/** The panel: no padding of its own on the sides (the list sets the gutters), a little above and below. */
const searchPanel = cx("flex-col rounded-(--radius-lg) border border-border bg-popover py-[6px] shadow-lg", popoverMotion.base);

const resultRow = cx("relative flex-row items-center gap-(--space-3) min-h-[58px] border-b border-border pl-[8px] pr-[10px] py-[7px] hover:bg-muted", transitionClass);

/** In a row's leading gutter: a primary bar while it is highlighted. Takes no room in the row's layout. */
function SelectionBar(props: { selected: boolean }) {
  return (
    <If when={props.selected}>
      <View class="absolute left-[2px] top-[10px] bottom-[10px] w-[3px] rounded-full bg-primary" accessibilityElementsHidden />
    </If>
  );
}

// ---------------------------------------------------------------- extension colours

/**
 * The families a file extension's badge is coloured by: each a soft fill and
 * a high-contrast ink from the theme (`ext-*` in theme/theme.json, light and
 * dark), the same for every file of the family. The two video formats get
 * related tints (MP4 violet, WebM indigo). Anything unknown is neutral.
 */
export type ExtensionTone = "text" | "markdown" | "pdf" | "word" | "image" | "audio" | "video" | "webm" | "neutral";

const EXTENSION_TONES: Record<string, ExtensionTone> = {
  txt: "text",
  text: "text",
  md: "markdown",
  markdown: "markdown",
  pdf: "pdf",
  doc: "word",
  docx: "word",
  png: "image",
  jpg: "image",
  jpeg: "image",
  gif: "image",
  webp: "image",
  wav: "audio",
  mp3: "audio",
  m4a: "audio",
  ogg: "audio",
  flac: "audio",
  mp4: "video",
  m4v: "video",
  mov: "video",
  webm: "webm",
};

/** The colour family of an extension (".JPG" or "jpg"), case-insensitive. */
export function extensionTone(ext: string): ExtensionTone {
  return EXTENSION_TONES[ext.replace(/^\./, "").toLowerCase()] ?? "neutral";
}

const extensionFill: Record<ExtensionTone, string> = {
  text: "bg-ext-text",
  markdown: "bg-ext-markdown",
  pdf: "bg-ext-pdf",
  word: "bg-ext-word",
  image: "bg-ext-image",
  audio: "bg-ext-audio",
  video: "bg-ext-video",
  webm: "bg-ext-webm",
  neutral: "bg-tone-neutral",
};

const extensionInk: Record<ExtensionTone, string> = {
  text: "text-ext-text-foreground",
  markdown: "text-ext-markdown-foreground",
  pdf: "text-ext-pdf-foreground",
  word: "text-ext-word-foreground",
  image: "text-ext-image-foreground",
  audio: "text-ext-audio-foreground",
  video: "text-ext-video-foreground",
  webm: "text-ext-webm-foreground",
  neutral: "text-tone-neutral-foreground",
};

/** A file's extension as a small badge in its family's colours, shown as the file's name has it. Decorative: the row's name says it. */
export function ExtensionBadge(props: { ext: string; testID?: string }) {
  return (
    <View class={cx("shrink-0 rounded-[5px] px-[6px] py-px", extensionFill[extensionTone(props.ext)])} testID={props.testID} accessibilityElementsHidden>
      <Text class={cx(textClass.xs, "font-semibold font-mono", extensionInk[extensionTone(props.ext)])}>{props.ext}</Text>
    </View>
  );
}

function RowTitle(props: { text: string; selected: boolean; weight: "semibold" | "normal" }) {
  return (
    <Text
      class={cx(textClass.sm, props.weight === "semibold" ? "font-semibold" : "font-normal", props.selected ? "text-accent-foreground" : "text-popover-foreground")}
      style={{ numberOfLines: 1 }}
    >
      {props.text}
    </Text>
  );
}

/**
 * A quick result for a message: its place's glyph in the leading column, one
 * line of its text, then who and where, quieter.
 */
export function SearchResultRow(
  props: Testable & {
    preview: JSX.Element;
    label: string;
    meta: string;
    selected: boolean;
    onPress: () => void;
  },
) {
  const row = revealWhenSelected(() => props.selected);
  return (
    <View
      ref={row}
      class={cx(resultRow, props.selected && "bg-accent")}
      focusable={false}
      role="option"
      accessibilityState={{ selected: props.selected }}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? `${props.label}, ${props.meta}`}
    >
      <SelectionBar selected={props.selected} />
      <View class="w-[72px] shrink-0 items-center justify-center" accessibilityElementsHidden>
        {props.preview}
      </View>
      <View class="flex-1 min-w-0 flex-col gap-px">
        <RowTitle text={props.label} selected={props.selected} weight="normal" />
        <Text class={cx(textClass.xs, "text-muted-foreground")} style={{ numberOfLines: 1 }}>
          {props.meta}
        </Text>
      </View>
    </View>
  );
}

/**
 * A quick result for a file: its tile, its name (without the extension) as
 * the strongest text with an optional quiet line under it (the start of its
 * text, its tags, a length), then on the right its source when it is needed
 * to tell two files apart, and its extension as a badge.
 */
export function SearchFileRow(
  props: Testable & {
    tile: JSX.Element;
    title: string;
    detail?: JSX.Element;
    /** The extension with its dot (".jpg"), or empty. */
    badge?: string;
    source?: string;
    selected: boolean;
    onPress: () => void;
  },
) {
  const row = revealWhenSelected(() => props.selected);
  return (
    <View
      ref={row}
      class={cx(resultRow, props.selected && "bg-accent")}
      focusable={false}
      role="option"
      accessibilityState={{ selected: props.selected }}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? props.title}
    >
      <SelectionBar selected={props.selected} />
      {props.tile}
      <View class="flex-1 min-w-0 flex-col gap-px">
        <RowTitle text={props.title} selected={props.selected} weight="semibold" />
        {props.detail}
      </View>
      <If when={props.source}>
        <Text class={cx("max-w-[112px]", textClass.xs, "text-muted-foreground")} style={{ numberOfLines: 1 }}>
          {props.source}
        </Text>
      </If>
      <View class="w-[52px] shrink-0 items-end">
        <If when={props.badge}>{(ext) => <ExtensionBadge ext={ext()} testID={props.testID ? `${props.testID}:ext` : undefined} />}</If>
      </View>
    </View>
  );
}

/** A heading over a group of results (Files, Messages): not a row, never highlighted. */
export function SearchGroupHeader(props: Testable & { label: string }) {
  return (
    <View class="pl-[8px] pr-[10px] pt-[10px] pb-[4px]" testID={props.testID} role="header">
      <Text class={cx(textClass.xs, "font-semibold text-muted-foreground")}>{props.label}</Text>
    </View>
  );
}

/** A row's ref that scrolls the list just enough to show the row while it is highlighted (Up and Down). */
function revealWhenSelected(selected: () => boolean): (node: ViewRef) => void {
  let node: ViewRef | undefined;
  effect(() => {
    if (!selected()) return;
    // Once laid out (measure waits for this turn's layout); a row not drawn yet has nothing to reveal.
    let live = true;
    void node?.measure().then((m) => {
      if (live && m && m.bounds.size.height > 0) node?.reveal({ vertical: "nearest", animated: !untracked(reduceMotion) });
    });
    return () => {
      live = false;
    };
  });
  return (n) => (node = n);
}

/** A message's or a place's glyph, as a quick result's preview. */
export function SearchResultGlyph(props: { icon: IconName }) {
  return (
    <View class="size-[28px] items-center justify-center rounded-(--radius-sm) bg-muted">
      <Icon name={props.icon} color="muted" size={16} />
    </View>
  );
}

/** A line in place of results: searching, nothing found, a failure, not available. */
export function SearchResultStatus(props: Testable & { text: string; tone?: "muted" | "danger" }) {
  return (
    <View class="pl-[8px] pr-[10px] py-(--space-3) border-b border-border" testID={props.testID} role={props.tone === "danger" ? "alert" : undefined}>
      <Text class={cx(textClass.sm, props.tone === "danger" ? "text-tone-danger-foreground" : "text-muted-foreground")}>{props.text}</Text>
    </View>
  );
}

/** The list's footer, always last: more results in full Search. */
export function SearchMoreRow(props: Testable & { label: string; selected: boolean; onPress: () => void }) {
  const row = revealWhenSelected(() => props.selected);
  return (
    <View
      ref={row}
      class={cx("relative flex-row items-center gap-(--space-2) mt-[6px] rounded-(--radius-sm) pl-[8px] pr-[10px] py-[9px] hover:bg-accent", transitionClass, props.selected ? "bg-accent" : "bg-muted")}
      focusable={false}
      role="option"
      accessibilityState={{ selected: props.selected }}
      onPress={props.onPress}
      testID={props.testID}
      accessibilityLabel={props.accessibilityLabel ?? props.label}
    >
      <SelectionBar selected={props.selected} />
      <Icon name="search" color="muted" />
      <Text class={cx("flex-1", textClass.sm, "font-medium", props.selected ? "text-accent-foreground" : "text-popover-foreground")}>{props.label}</Text>
      <Icon name="chevron-right" color="muted" size={14} />
    </View>
  );
}
