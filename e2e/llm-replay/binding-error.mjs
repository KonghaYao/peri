export class BindingError extends Error {
  constructor(code, message) {
    super(message);
    this.name = 'BindingError';
    this.code = code;
  }
}
