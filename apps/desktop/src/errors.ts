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
  if (message.includes('No relay of the public network accepted a reservation')) return 'Не удалось подключиться к публичной сети. Проверьте доступ в интернет и повторите; если не поможет, понадобится свой relay-сервер.';
  if (message.includes('no longer lets this computer in')) return 'Друг больше не впускает вас без кода. Попросите у него новый код.';
  if (message.includes("The friend's world was not found")) return 'Мир друга сейчас не найден в сети. Попросите друга открыть мир и нажать «Пригласить друга», затем попробуйте снова.';
  if (message.includes('No host with this code was found')) return 'Друг с таким кодом не найден в сети. Проверьте код: у друга BlockOps должен быть открыт, а код ещё не использован.';
  if (message.includes('does not match this host') || message.includes('The host refused access')) return 'Код не подошёл. Попросите друга создать новый.';
  if (message.includes('Direct connection failed')) return 'Не удалось соединиться с другом напрямую: ваши сети этого не позволяют. Так бывает с мобильным интернетом и у некоторых провайдеров; для таких случаев понадобится свой relay-сервер.';
  if (message.includes('error sending request')) return 'Не удалось связаться с сервисом координации. Проверьте его адрес в дополнительных настройках подключения и доступ в интернет.';
  return message;
}
