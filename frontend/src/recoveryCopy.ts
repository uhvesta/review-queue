import contractJson from "./recovery-copy-contract.json";

export interface RecoveryCopy {
  state: string;
  codes: string[];
  what_happened: string;
  why_it_matters: string;
  data_safety: string;
  next_action: string;
  diagnostics_route: string;
  cancel_route: string;
}

interface RecoveryCopyContract {
  schema_version: number;
  contract: string;
  entries: RecoveryCopy[];
}

export const recoveryCopyContract = contractJson as RecoveryCopyContract;

const byCode = new Map(
  recoveryCopyContract.entries.flatMap((entry) =>
    entry.codes.map((code) => [code, entry] as const),
  ),
);

export function recoveryCopyFor(code: string): RecoveryCopy | undefined {
  return byCode.get(code);
}

export function recoveryCopyProblems(entry: RecoveryCopy): string[] {
  const problems: string[] = [];
  const required = [
    ["what_happened", entry.what_happened],
    ["why_it_matters", entry.why_it_matters],
    ["data_safety", entry.data_safety],
    ["next_action", entry.next_action],
    ["diagnostics_route", entry.diagnostics_route],
    ["cancel_route", entry.cancel_route],
  ] as const;
  for (const [field, value] of required) {
    if (!value.trim()) problems.push(`${entry.state}.${field} is empty`);
  }
  if (entry.codes.length === 0) problems.push(`${entry.state}.codes is empty`);
  if (
    /^(something went wrong|an error occurred|review queue could not complete that action|try again|retry|contact support)\.?$/i.test(
      entry.what_happened.trim(),
    )
  ) {
    problems.push(`${entry.state}.what_happened is generic`);
  }
  if (
    /^(try again|retry|retry the action|contact support)\.?$/i.test(
      entry.next_action.trim(),
    )
  ) {
    problems.push(`${entry.state}.next_action is generic`);
  }
  return problems;
}
