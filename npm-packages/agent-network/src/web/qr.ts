/**
 * 浏览器端二维码编码器（QR Code Model 2）。
 *
 * 以脚本字符串导出，由 `src/web/chat.ts` 内联进页面；页面不请求任何外部
 * 二维码 API。实现范围：字节模式、纠错等级 L、版本 1–12、8 种掩码与
 * ISO/IEC 18004 惩罚评分。版本 12-L 的字节容量为 322 字节，覆盖当前
 * `origin + /#t= + token` 的长度。
 *
 * 只暴露 `window.agentNetworkQr`；其余符号封装在 IIFE 内，避免与页面
 * 主脚本的全局函数重名。
 */
export const QR_ENCODER_SCRIPT = `
(function () {
  'use strict';

  // 每个版本的纠错等级 L 分块：每块数据码字数。同一版本可能有 2 种块长度。
  var BLOCK_DATA = [
    null,
    [19],
    [34],
    [55],
    [80],
    [108],
    [68, 68],
    [78, 78],
    [97, 97],
    [116, 116],
    [68, 68, 69, 69],
    [81, 81, 81, 81],
    [92, 92, 93, 93]
  ];
  // 每块纠错码字数，索引与 BLOCK_DATA 对齐。
  var EC_PER_BLOCK = [0, 7, 10, 15, 20, 26, 18, 20, 24, 30, 18, 20, 24];
  // 对齐图案中心坐标；版本 1 没有对齐图案。
  var ALIGN = [
    null,
    [],
    [6, 18],
    [6, 22],
    [6, 26],
    [6, 30],
    [6, 34],
    [6, 22, 38],
    [6, 24, 42],
    [6, 26, 46],
    [6, 28, 50],
    [6, 30, 54],
    [6, 32, 58]
  ];

  var EXP = new Array(512);
  var LOG = new Array(256);
  (function initGaloisField() {
    var x = 1;
    for (var i = 0; i < 255; i += 1) {
      EXP[i] = x;
      LOG[x] = i;
      x <<= 1;
      if (x & 0x100) x ^= 0x11d;
    }
    for (var j = 255; j < 512; j += 1) EXP[j] = EXP[j - 255];
  })();

  function gfMul(a, b) {
    if (a === 0 || b === 0) return 0;
    return EXP[LOG[a] + LOG[b]];
  }

  function bchDigit(value) {
    var digit = 0;
    while (value !== 0) {
      digit += 1;
      value >>>= 1;
    }
    return digit;
  }

  var G15 = 0x537;
  var G15_MASK = 0x5412;
  var G18 = 0x1f25;

  function typeInfoBits(mask) {
    // 纠错等级 L 的两位编码为 01。
    var data = (1 << 3) | mask;
    var d = data << 10;
    while (bchDigit(d) - bchDigit(G15) >= 0) {
      d ^= G15 << (bchDigit(d) - bchDigit(G15));
    }
    return ((data << 10) | d) ^ G15_MASK;
  }

  function versionInfoBits(version) {
    var d = version << 12;
    while (bchDigit(d) - bchDigit(G18) >= 0) {
      d ^= G18 << (bchDigit(d) - bchDigit(G18));
    }
    return (version << 12) | d;
  }

  function dataCodewords(version) {
    var blocks = BLOCK_DATA[version];
    var total = 0;
    for (var i = 0; i < blocks.length; i += 1) total += blocks[i];
    return total;
  }

  function byteCapacity(version) {
    var totalBits = dataCodewords(version) * 8;
    var countBits = version < 10 ? 8 : 16;
    return Math.floor((totalBits - 4 - countBits) / 8);
  }

  function pickVersion(byteLength) {
    for (var version = 1; version < BLOCK_DATA.length; version += 1) {
      if (byteLength <= byteCapacity(version)) return version;
    }
    return 0;
  }

  function makeCodewords(bytes, version) {
    var total = dataCodewords(version);
    var totalBits = total * 8;
    var bits = [];
    function push(value, length) {
      for (var i = length - 1; i >= 0; i -= 1) bits.push((value >>> i) & 1);
    }
    push(4, 4);
    push(bytes.length, version < 10 ? 8 : 16);
    for (var i = 0; i < bytes.length; i += 1) push(bytes[i], 8);
    var terminator = 0;
    while (terminator < 4 && bits.length < totalBits) {
      bits.push(0);
      terminator += 1;
    }
    while (bits.length % 8 !== 0) bits.push(0);

    var codewords = [];
    for (var offset = 0; offset < bits.length; offset += 8) {
      var value = 0;
      for (var bit = 0; bit < 8; bit += 1) value = (value << 1) | bits[offset + bit];
      codewords.push(value);
    }
    var pads = [0xec, 0x11];
    var padIndex = 0;
    while (codewords.length < total) {
      codewords.push(pads[padIndex]);
      padIndex = padIndex === 0 ? 1 : 0;
    }
    return codewords;
  }

  /** Reed-Solomon：生成多项式与综合除法，返回 ec 字节。 */
  function reedSolomon(data, ecLength) {
    var generator = [1];
    for (var i = 0; i < ecLength; i += 1) {
      var alpha = EXP[i];
      var next = new Array(generator.length + 1);
      for (var k = 0; k < next.length; k += 1) next[k] = 0;
      for (var j = 0; j < generator.length; j += 1) {
        next[j] ^= generator[j];
        next[j + 1] ^= gfMul(generator[j], alpha);
      }
      generator = next;
    }
    var result = data.slice();
    for (var pad = 0; pad < ecLength; pad += 1) result.push(0);
    for (var index = 0; index < data.length; index += 1) {
      var coefficient = result[index];
      if (coefficient === 0) continue;
      for (var g = 0; g < generator.length; g += 1) {
        result[index + g] ^= gfMul(generator[g], coefficient);
      }
    }
    return result.slice(data.length);
  }

  function interleave(version, codewords) {
    var blockLengths = BLOCK_DATA[version];
    var ecLength = EC_PER_BLOCK[version];
    var blocks = [];
    var offset = 0;
    for (var i = 0; i < blockLengths.length; i += 1) {
      var length = blockLengths[i];
      var data = codewords.slice(offset, offset + length);
      offset += length;
      blocks.push({ data: data, ec: reedSolomon(data, ecLength) });
    }
    var output = [];
    var maxLength = 0;
    for (var b = 0; b < blocks.length; b += 1) {
      if (blocks[b].data.length > maxLength) maxLength = blocks[b].data.length;
    }
    for (var position = 0; position < maxLength; position += 1) {
      for (var block = 0; block < blocks.length; block += 1) {
        if (position < blocks[block].data.length) output.push(blocks[block].data[position]);
      }
    }
    for (var ec = 0; ec < ecLength; ec += 1) {
      for (var item = 0; item < blocks.length; item += 1) output.push(blocks[item].ec[ec]);
    }
    return output;
  }

  function maskBit(mask, row, column) {
    switch (mask) {
      case 0:
        return (row + column) % 2 === 0;
      case 1:
        return row % 2 === 0;
      case 2:
        return column % 3 === 0;
      case 3:
        return (row + column) % 3 === 0;
      case 4:
        return (Math.floor(row / 2) + Math.floor(column / 3)) % 2 === 0;
      case 5:
        return ((row * column) % 2) + ((row * column) % 3) === 0;
      case 6:
        return (((row * column) % 2) + ((row * column) % 3)) % 2 === 0;
      default:
        return (((row + column) % 2) + ((row * column) % 3)) % 2 === 0;
    }
  }

  function createMatrix(version) {
    var size = version * 4 + 17;
    var modules = [];
    var reserved = [];
    for (var row = 0; row < size; row += 1) {
      var moduleRow = [];
      var reservedRow = [];
      for (var column = 0; column < size; column += 1) {
        moduleRow.push(false);
        reservedRow.push(false);
      }
      modules.push(moduleRow);
      reserved.push(reservedRow);
    }

    function set(row, column, dark) {
      if (row < 0 || column < 0 || row >= size || column >= size) return;
      modules[row][column] = dark;
      reserved[row][column] = true;
    }

    function finder(top, left) {
      for (var dr = -1; dr <= 7; dr += 1) {
        for (var dc = -1; dc <= 7; dc += 1) {
          var inside = dr >= 0 && dr <= 6 && dc >= 0 && dc <= 6;
          var dark =
            inside &&
            (dr === 0 || dr === 6 || dc === 0 || dc === 6 ||
              (dr >= 2 && dr <= 4 && dc >= 2 && dc <= 4));
          set(top + dr, left + dc, dark);
        }
      }
    }

    finder(0, 0);
    finder(0, size - 7);
    finder(size - 7, 0);

    for (var timing = 8; timing < size - 8; timing += 1) {
      set(6, timing, timing % 2 === 0);
      set(timing, 6, timing % 2 === 0);
    }

    var centers = ALIGN[version];
    for (var a = 0; a < centers.length; a += 1) {
      for (var b = 0; b < centers.length; b += 1) {
        var centerRow = centers[a];
        var centerColumn = centers[b];
        var overlapsFinder =
          (centerRow === 6 && centerColumn === 6) ||
          (centerRow === 6 && centerColumn === size - 7) ||
          (centerRow === size - 7 && centerColumn === 6);
        if (overlapsFinder) continue;
        for (var ar = -2; ar <= 2; ar += 1) {
          for (var ac = -2; ac <= 2; ac += 1) {
            var distance = Math.max(Math.abs(ar), Math.abs(ac));
            set(centerRow + ar, centerColumn + ac, distance !== 1);
          }
        }
      }
    }

    // 固定暗模块。
    set(size - 8, 8, true);

    // 预留格式信息区（两份）与版本信息区。
    for (var info = 0; info < 15; info += 1) {
      var infoRow = info < 6 ? info : info < 8 ? info + 1 : size - 15 + info;
      reserved[infoRow][8] = true;
      var infoColumn = info < 8 ? size - 1 - info : info === 8 ? 7 : 15 - info - 1;
      reserved[8][infoColumn] = true;
    }
    if (version >= 7) {
      for (var versionBit = 0; versionBit < 18; versionBit += 1) {
        var versionRow = Math.floor(versionBit / 3);
        var versionColumn = (versionBit % 3) + size - 11;
        reserved[versionRow][versionColumn] = true;
        reserved[versionColumn][versionRow] = true;
      }
    }

    return { size: size, modules: modules, reserved: reserved };
  }

  function writeFormat(matrix, mask) {
    var bits = typeInfoBits(mask);
    for (var i = 0; i < 15; i += 1) {
      var dark = ((bits >> i) & 1) === 1;
      var row = i < 6 ? i : i < 8 ? i + 1 : matrix.size - 15 + i;
      matrix.modules[row][8] = dark;
      var column = i < 8 ? matrix.size - 1 - i : i === 8 ? 7 : 15 - i - 1;
      matrix.modules[8][column] = dark;
    }
    matrix.modules[matrix.size - 8][8] = true;
  }

  function writeVersion(matrix, version) {
    if (version < 7) return;
    var bits = versionInfoBits(version);
    for (var i = 0; i < 18; i += 1) {
      var dark = ((bits >> i) & 1) === 1;
      matrix.modules[Math.floor(i / 3)][(i % 3) + matrix.size - 11] = dark;
      matrix.modules[(i % 3) + matrix.size - 11][Math.floor(i / 3)] = dark;
    }
  }

  function placeData(matrix, codewords, mask) {
    var size = matrix.size;
    var upward = true;
    var row = size - 1;
    var bitIndex = 7;
    var byteIndex = 0;
    for (var column = size - 1; column > 0; column -= 2) {
      if (column === 6) column -= 1;
      for (;;) {
        for (var offset = 0; offset < 2; offset += 1) {
          var targetColumn = column - offset;
          if (matrix.reserved[row][targetColumn]) continue;
          var dark = false;
          if (byteIndex < codewords.length) {
            dark = ((codewords[byteIndex] >>> bitIndex) & 1) === 1;
          }
          if (maskBit(mask, row, targetColumn)) dark = !dark;
          matrix.modules[row][targetColumn] = dark;
          bitIndex -= 1;
          if (bitIndex === -1) {
            byteIndex += 1;
            bitIndex = 7;
          }
        }
        row += upward ? -1 : 1;
        if (row < 0 || row >= size) {
          row += upward ? 1 : -1;
          upward = !upward;
          break;
        }
      }
    }
  }

  var FINDER_PATTERNS = [
    [true, false, true, true, true, false, true, false, false, false, false],
    [false, false, false, false, true, false, true, true, true, false, true]
  ];

  function penalty(matrix) {
    var size = matrix.size;
    var modules = matrix.modules;
    var score = 0;
    var row;
    var column;

    for (row = 0; row < size; row += 1) {
      var runColor = modules[row][0];
      var runLength = 1;
      for (column = 1; column < size; column += 1) {
        if (modules[row][column] === runColor) {
          runLength += 1;
        } else {
          if (runLength >= 5) score += 3 + runLength - 5;
          runColor = modules[row][column];
          runLength = 1;
        }
      }
      if (runLength >= 5) score += 3 + runLength - 5;
    }
    for (column = 0; column < size; column += 1) {
      var columnColor = modules[0][column];
      var columnRun = 1;
      for (row = 1; row < size; row += 1) {
        if (modules[row][column] === columnColor) {
          columnRun += 1;
        } else {
          if (columnRun >= 5) score += 3 + columnRun - 5;
          columnColor = modules[row][column];
          columnRun = 1;
        }
      }
      if (columnRun >= 5) score += 3 + columnRun - 5;
    }
    for (row = 0; row < size - 1; row += 1) {
      for (column = 0; column < size - 1; column += 1) {
        var first = modules[row][column];
        if (
          first === modules[row][column + 1] &&
          first === modules[row + 1][column] &&
          first === modules[row + 1][column + 1]
        ) {
          score += 3;
        }
      }
    }
    for (row = 0; row < size; row += 1) {
      for (column = 0; column + 11 <= size; column += 1) {
        for (var pattern = 0; pattern < FINDER_PATTERNS.length; pattern += 1) {
          var rowMatch = true;
          for (var index = 0; index < 11; index += 1) {
            if (modules[row][column + index] !== FINDER_PATTERNS[pattern][index]) {
              rowMatch = false;
              break;
            }
          }
          if (rowMatch) score += 40;
        }
      }
    }
    for (column = 0; column < size; column += 1) {
      for (row = 0; row + 11 <= size; row += 1) {
        for (var verticalPattern = 0; verticalPattern < FINDER_PATTERNS.length; verticalPattern += 1) {
          var columnMatch = true;
          for (var verticalIndex = 0; verticalIndex < 11; verticalIndex += 1) {
            if (modules[row + verticalIndex][column] !== FINDER_PATTERNS[verticalPattern][verticalIndex]) {
              columnMatch = false;
              break;
            }
          }
          if (columnMatch) score += 40;
        }
      }
    }
    var darkCount = 0;
    for (row = 0; row < size; row += 1) {
      for (column = 0; column < size; column += 1) {
        if (modules[row][column]) darkCount += 1;
      }
    }
    var percent = (darkCount * 100) / (size * size);
    score += Math.floor(Math.abs(percent - 50) / 5) * 10;
    return score;
  }

  /** 编码文本，返回 { size, modules }；内容超出容量时抛错。 */
  function encode(text) {
    if (typeof text !== 'string' || text.length === 0) {
      throw new Error('二维码内容为空');
    }
    var bytes = new TextEncoder().encode(text);
    var version = pickVersion(bytes.length);
    if (version === 0) throw new Error('二维码内容过长');
    var codewords = interleave(version, makeCodewords(bytes, version));
    var best = null;
    for (var mask = 0; mask < 8; mask += 1) {
      var matrix = createMatrix(version);
      writeFormat(matrix, mask);
      writeVersion(matrix, version);
      placeData(matrix, codewords, mask);
      var score = penalty(matrix);
      if (best === null || score < best.score) {
        best = { size: matrix.size, modules: matrix.modules, score: score };
      }
    }
    return { size: best.size, modules: best.modules };
  }

  /** 把二维码画到 canvas；白底黑块，四周留 4 模块静区。 */
  function draw(canvas, text) {
    var result = encode(text);
    var quiet = 4;
    var totalModules = result.size + quiet * 2;
    var scale = Math.max(2, Math.ceil(280 / totalModules));
    canvas.width = totalModules * scale;
    canvas.height = totalModules * scale;
    var context = canvas.getContext('2d');
    if (!context) throw new Error('浏览器不支持 canvas');
    context.fillStyle = '#ffffff';
    context.fillRect(0, 0, canvas.width, canvas.height);
    context.fillStyle = '#111111';
    for (var row = 0; row < result.size; row += 1) {
      for (var column = 0; column < result.size; column += 1) {
        if (!result.modules[row][column]) continue;
        context.fillRect(
          (column + quiet) * scale,
          (row + quiet) * scale,
          scale,
          scale
        );
      }
    }
  }

  window.agentNetworkQr = { encode: encode, draw: draw };
})();
`;
