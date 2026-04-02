import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { PlanInput } from "./PlanInput";

describe("PlanInput (UT-04)", () => {
  it("UT-04.1: empty input → button disabled", () => {
    const onPlan = vi.fn();
    render(<PlanInput onPlan={onPlan} loading={false} />);

    const btn = screen.getByRole("button", { name: /plan mission/i });
    expect(btn).toBeDisabled();
  });

  it("UT-04.2: input text → button enabled", () => {
    const onPlan = vi.fn();
    render(<PlanInput onPlan={onPlan} loading={false} />);

    const textarea = screen.getByPlaceholderText(/describe your mission/i);
    fireEvent.change(textarea, { target: { value: "实现用户认证" } });

    const btn = screen.getByRole("button", { name: /plan mission/i });
    expect(btn).not.toBeDisabled();
  });

  it("UT-04.3: character limit 2000", () => {
    const onPlan = vi.fn();
    render(<PlanInput onPlan={onPlan} loading={false} />);

    const textarea = screen.getByPlaceholderText(/describe your mission/i);
    const longText = "x".repeat(2500);
    fireEvent.change(textarea, { target: { value: longText } });

    expect((textarea as HTMLTextAreaElement).value.length).toBeLessThanOrEqual(2000);
    expect(screen.getByText("2000/2000")).toBeInTheDocument();
  });

  it("UT-04.4: loading state → button shows Planning..., input disabled", () => {
    const onPlan = vi.fn();
    render(<PlanInput onPlan={onPlan} loading={true} />);

    const btn = screen.getByRole("button", { name: /planning/i });
    expect(btn).toBeDisabled();

    const textarea = screen.getByPlaceholderText(/describe your mission/i);
    expect(textarea).toBeDisabled();
  });

  it("UT-04.5: Cmd+Enter triggers submit", () => {
    const onPlan = vi.fn();
    render(<PlanInput onPlan={onPlan} loading={false} />);

    const textarea = screen.getByPlaceholderText(/describe your mission/i);
    fireEvent.change(textarea, { target: { value: "test" } });
    fireEvent.keyDown(textarea, { key: "Enter", metaKey: true });

    expect(onPlan).toHaveBeenCalledWith("test");
  });

  it("click Plan Mission triggers onPlan", () => {
    const onPlan = vi.fn();
    render(<PlanInput onPlan={onPlan} loading={false} />);

    const textarea = screen.getByPlaceholderText(/describe your mission/i);
    fireEvent.change(textarea, { target: { value: "build auth" } });

    const btn = screen.getByRole("button", { name: /plan mission/i });
    fireEvent.click(btn);

    expect(onPlan).toHaveBeenCalledWith("build auth");
  });
});
