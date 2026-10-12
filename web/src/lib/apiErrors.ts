import { ApiError } from "./api";

export function isAuthStatus(err: unknown): boolean {
  return err instanceof ApiError && (err.status === 401 || err.status === 403);
}

export function describeApiError(err: unknown): string {
  if (err instanceof ApiError) {
    if (isAuthStatus(err)) return "You need to be logged in for that";
    return err.message;
  }
  return "Something went wrong, please retry";
}
