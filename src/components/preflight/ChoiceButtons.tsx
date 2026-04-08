import { useState } from "react";
import type { PreflightChoice } from "../../ipc/commands";
import styles from "./ChoiceButtons.module.css";

interface ChoiceButtonsProps {
  choices: PreflightChoice[];
  onSelect: (choice: PreflightChoice) => void;
  disabled?: boolean;
}

export function ChoiceButtons({ choices, onSelect, disabled }: ChoiceButtonsProps) {
  const [selectedId, setSelectedId] = useState<string | null>(null);

  if (choices.length === 0) return null;

  const handleClick = (choice: PreflightChoice) => {
    if (selectedId || disabled) return;
    setSelectedId(choice.id);
    onSelect(choice);
  };

  return (
    <div className={styles.container}>
      {choices.map((choice) => {
        const isSelected = selectedId === choice.id;
        const isDimmed = selectedId !== null && !isSelected;

        let className = styles.choiceBtn;
        if (isSelected) className += ` ${styles.selected}`;
        if (isDimmed) className += ` ${styles.dimmed}`;

        return (
          <button
            key={choice.id}
            className={className}
            onClick={() => handleClick(choice)}
            disabled={!!selectedId || disabled}
          >
            <span className={styles.choiceKey}>{choice.id}</span>
            <span>{choice.label}</span>
          </button>
        );
      })}
    </div>
  );
}
