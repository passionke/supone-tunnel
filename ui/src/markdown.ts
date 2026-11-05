import DOMPurify from "dompurify";
import { marked } from "marked";

const renderer = new marked.Renderer();
renderer.link = ({ href, title, text }) => {
  const url = href ?? "";
  const titleAttr = title ? ` title="${escapeAttr(title)}"` : "";
  return `<a href="${escapeAttr(url)}"${titleAttr} target="_blank" rel="noopener noreferrer">${text}</a>`;
};

marked.use({
  gfm: true,
  breaks: true,
  renderer,
});

export function renderMarkdown(source: string): string {
  const html = marked.parse(source, { async: false });
  return DOMPurify.sanitize(html, {
    ADD_ATTR: ["target", "rel"],
  });
}

function escapeAttr(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll('"', "&quot;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");
}
