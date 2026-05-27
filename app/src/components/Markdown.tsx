import React from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { open as shellOpen } from "@tauri-apps/plugin-shell";

interface MarkdownProps {
  text: string;
  variant?: "assistant" | "user";
}

// Route link clicks through the OS shell so they open in the user's default
// browser instead of navigating the Tauri webview (which is locked to the
// app's own origin and would block the navigation outright).
function ExternalLink({ href, children, ...rest }: React.AnchorHTMLAttributes<HTMLAnchorElement>) {
  const onClick = (e: React.MouseEvent<HTMLAnchorElement>) => {
    if (!href) return;
    e.preventDefault();
    void shellOpen(href).catch(() => {
      // Swallow failures silently — the link click already prevented default
      // and the webview navigation guard would block any fallback anyway.
    });
  };
  return (
    <a href={href} rel="noopener noreferrer" onClick={onClick} {...rest}>
      {children}
    </a>
  );
}

const MARKDOWN_COMPONENTS = { a: ExternalLink } as const;

export const Markdown = React.memo(function Markdown({ text, variant = "assistant" }: MarkdownProps) {
  return (
    <div className={`markdown-body ${variant === "user" ? "markdown-user" : "markdown-asst"}`}>
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={MARKDOWN_COMPONENTS}>{text}</ReactMarkdown>
    </div>
  );
});
