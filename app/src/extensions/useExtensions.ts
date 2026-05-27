import { useContext } from "react";
import { ExtensionReactContext } from "./ExtensionProvider";

/** Consume the ExtensionProvider context. Throws if used outside the
 *  provider — `<ExtensionProvider>` must wrap the app root. */
export function useExtensions() {
  const ctx = useContext(ExtensionReactContext);
  if (!ctx) {
    throw new Error(
      "useExtensions() must be used inside <ExtensionProvider>",
    );
  }
  return ctx;
}
