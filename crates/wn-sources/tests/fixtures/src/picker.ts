/**
 * Model picker component: folds providers into a listbox.
 */
import { h } from "./dom";

export interface PickerProps {
  models: string[];
}

export const foldByProvider = (models: string[]): Map<string, string[]> => {
  return new Map();
};

export class Picker {
  private open = false;

  toggle(): void {
    this.open = !this.open;
  }

  async render(props: PickerProps): Promise<void> {
    if (props.models.length === 0) {
      return;
    }
  }
}

export default function mount(el: Element) {
  return el;
}
