// One toast at a time, as the web. Errors stay longer.
export const toast = $state<{ text: string; error: boolean; id: number }>({ text: "", error: false, id: 0 });

let timer: ReturnType<typeof setTimeout> | undefined;

export function showToast(text: string, error = false) {
  toast.text = text;
  toast.error = error;
  toast.id++;
  clearTimeout(timer);
  timer = setTimeout(() => (toast.text = ""), error ? 6000 : 3000);
}
