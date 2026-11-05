/**
 * A modal confirmation (a native <dialog>): a title, a sentence, Cancel and the action. Esc,
 * Cancel or a click outside the box say no. The focus starts on Cancel, so Enter never deletes
 * by accident, and goes back where it was afterwards.
 */
export interface ConfirmOptions {
  title: string;
  body: string;
  /** The action button's text ("Delete"). */
  action: string;
  /** The action destroys something: a red button. */
  danger?: boolean;
}

export function confirmDialog(o: ConfirmOptions): Promise<boolean> {
  const before = document.activeElement as HTMLElement | null;
  const dialog = document.createElement("dialog");
  dialog.className = "confirm";
  dialog.setAttribute("aria-labelledby", "confirm-title");
  dialog.setAttribute("aria-describedby", "confirm-body");
  const h = document.createElement("h2");
  h.id = "confirm-title";
  h.textContent = o.title;
  const p = document.createElement("p");
  p.id = "confirm-body";
  p.textContent = o.body;
  const actions = document.createElement("div");
  actions.className = "confirm-actions";
  const cancel = document.createElement("button");
  cancel.type = "button";
  cancel.className = "btn btn-outline";
  cancel.textContent = "Cancel";
  const ok = document.createElement("button");
  ok.type = "button";
  ok.className = o.danger ? "btn btn-destructive" : "btn btn-default";
  ok.textContent = o.action;
  actions.append(cancel, ok);
  dialog.append(h, p, actions);
  document.body.append(dialog);

  return new Promise((resolve) => {
    let answer = false;
    const close = (yes: boolean) => {
      answer = yes;
      dialog.close();
    };
    cancel.addEventListener("click", () => close(false));
    ok.addEventListener("click", () => close(true));
    // A click on the backdrop (the dialog element itself, outside its content box).
    dialog.addEventListener("click", (e) => {
      if (e.target !== dialog) return;
      const r = dialog.getBoundingClientRect();
      if (e.clientX < r.left || e.clientX > r.right || e.clientY < r.top || e.clientY > r.bottom) close(false);
    });
    dialog.addEventListener("close", () => {
      dialog.remove();
      before?.focus?.();
      resolve(answer);
    });
    dialog.showModal();
    cancel.focus();
  });
}
