// The web's setValue rule (reference/web/app.js): never overwrite a field someone is typing
// in. Rust normalises what it's sent (names are trimmed), and writing that back mid-word would
// eat the space just typed.
export function syncValue(node: HTMLInputElement, value: string | number) {
  node.value = String(value);
  return {
    update(v: string | number) {
      if (document.activeElement !== node) node.value = String(v);
    },
  };
}
