/** Turns backend and coordinator failures into advice a player can act on. */
export function explainError(error: unknown): string {
  const message = String(error);
  if (message.includes('404')) return 'Код не найден, уже использован или срок его действия истёк. Попросите друга создать новый.';
  if (message.includes('429')) return 'Слишком много попыток. Подождите минуту и попробуйте ещё раз.';
  if (message.includes('504')) return 'Сервер друга не подтвердил подключение. Попросите его создать новый код.';
  if (message.includes('Coordinator requires HTTPS')) return 'Адрес сервиса координации должен начинаться с https://. Адрес http:// допустим только для проверки на этом компьютере (127.0.0.1).';
  if (message.includes('Coordinator must be an origin URL') || message.includes('relative URL without a base') || message.includes('invalid domain') || message.includes('empty host')) {
    return 'Некорректный адрес сервиса координации. Укажите его без пути и параметров, например https://connect.example.org.';
  }
  if (message.includes('error sending request')) return 'Не удалось связаться с сервисом координации. Проверьте его адрес в дополнительных настройках подключения и доступ в интернет.';
  return message;
}
