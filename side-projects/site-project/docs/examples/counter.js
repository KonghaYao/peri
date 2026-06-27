// 示例：一个简单的计数器组件

function createCounter(initial: number = 0) {
  let count = initial;

  return {
    increment() {
      count++;
      return count;
    },
    decrement() {
      count--;
      return count;
    },
    reset() {
      count = initial;
      return count;
    },
    get value() {
      return count;
    }
  };
}

// 使用
const counter = createCounter(5);
console.log(counter.increment()); // 6
console.log(counter.increment()); // 7
console.log(counter.decrement()); // 6
console.log(counter.reset());     // 5
