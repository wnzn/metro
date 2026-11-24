// The only module feature code imports UI from. Swap the implementation behind
// these names (or theme/theme.json) to change the look without touching features.
//
// Importing it also starts the appearance (dark by default, then the saved
// preference) before the app's first frame: see ./appearance.
export * from "./appearance";
export * from "./panels";
import "./fonts/InterVariable.ttf";
export * from "./primitives";
export * from "./chrome";
export * from "./media";
export * from "./chat";
export type { Space, Tone } from "./tokens";
export * from "./settings";
export * from "./modelPicker";
