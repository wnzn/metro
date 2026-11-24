import { expect, test } from "nsh/test";
import { failureText } from "./failure";

test("plain failures stay as they are; technical ones become one sentence with the text kept for Details", () => {
  expect(failureText("workspace.read failed: runner offline")).toEqual({ sentence: "workspace.read failed: runner offline", details: null });
  const raw = 'The agent failed: CompletionError: ProviderError: HTTP 401 Unauthorized {"error":"bad key"}';
  expect(failureText(raw)).toEqual({ sentence: "The model provider refused the sign-in or key.", details: raw });
  expect(failureText("The agent failed: ProviderError: 429 Too Many Requests").sentence).toBe("The model provider is limiting requests right now.");
  expect(failureText("The agent failed: ProviderError: something odd").sentence).toBe("The agent failed.");
  expect(failureText('{"weird": true}').sentence).toBe("Something went wrong.");
  expect(failureText("").sentence).toBe("Something went wrong.");
});
