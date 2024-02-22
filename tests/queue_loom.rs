#[cfg(loom)]
mod loom {
    use ref_count::array_queue::ArrayQueue;
    use loom::thread;

    #[test]
    fn test_concurrent_push_pop() {
        loom::model(|| {
            let queue = ArrayQueue::<i32, 5>::new();
            let queue = std::sync::Arc::new(queue);

            let producer = {
                let queue = queue.clone();
                thread::spawn(move || {
                    queue.push(5).unwrap();
                })
            };

            let consumer = {
                let queue = queue.clone();
                thread::spawn(move || {
                    if let Some(item) = queue.pop() {
                        assert_eq!(item, 5);
                    }
                })
            };

            producer.join().unwrap();
            consumer.join().unwrap();
        });
    }
}