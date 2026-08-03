import React, { useState } from 'react';
import { Pressable, SafeAreaView, StyleSheet, Text, View } from 'react-native';

export default function App(): React.JSX.Element {
  const [revealed, setRevealed] = useState(false);
  const [fixedRevealed, setFixedRevealed] = useState(false);
  const [flash, setFlash] = useState(false);
  const [oneWay, setOneWay] = useState(false);

  const showTransientFlash = (): void => {
    setFlash(true);
    setTimeout(() => setFlash(false), 400);
  };

  return (
    <SafeAreaView style={[styles.screen, oneWay && styles.oneWay]} testID="screen">
      <View accessible={false} style={styles.card}>
        <Text accessibilityRole="header">Reproit React Native Fixture</Text>
        <Pressable
          accessibilityLabel="Toggle"
          accessibilityRole="button"
          testID="toggle"
          onPress={() => setRevealed((value) => !value)}
          style={styles.button}
        >
          <Text>Toggle</Text>
        </Pressable>
        {revealed ? <Text testID="detail">Detail revealed</Text> : null}
        <Pressable
          accessibilityLabel="Flash control"
          accessibilityRole="button"
          testID="flicker-positive"
          onPress={showTransientFlash}
          style={styles.button}
        >
          <Text>Flash control</Text>
        </Pressable>
        <Pressable
          accessibilityLabel="Fixed control"
          accessibilityRole="button"
          testID="flicker-fixed"
          onPress={() => setFixedRevealed(true)}
          style={styles.button}
        >
          <Text>Fixed control</Text>
        </Pressable>
        {fixedRevealed ? <Text>Fixed transition complete</Text> : null}
        <Pressable
          accessibilityLabel="One-way control"
          accessibilityRole="button"
          testID="flicker-one-way"
          onPress={() => setOneWay(true)}
          style={styles.button}
        >
          <Text>One-way control</Text>
        </Pressable>
      </View>
      {flash ? <View accessible={false} pointerEvents="none" style={styles.flash} /> : null}
    </SafeAreaView>
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1, justifyContent: 'center', padding: 24 },
  card: { gap: 20 },
  button: { padding: 16, backgroundColor: '#d8e8ff' },
  flash: { ...StyleSheet.absoluteFillObject, backgroundColor: '#000', zIndex: 1000 },
  oneWay: { backgroundColor: '#000' },
});
