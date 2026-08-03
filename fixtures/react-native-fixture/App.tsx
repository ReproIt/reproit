import React, { useState } from 'react';
import { Pressable, SafeAreaView, StyleSheet, Text, View } from 'react-native';

export default function App(): React.JSX.Element {
  const [revealed, setRevealed] = useState(false);

  return (
    <SafeAreaView style={styles.screen} testID="screen">
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
      </View>
    </SafeAreaView>
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1, justifyContent: 'center', padding: 24 },
  card: { gap: 20 },
  button: { padding: 16, backgroundColor: '#d8e8ff' },
});
